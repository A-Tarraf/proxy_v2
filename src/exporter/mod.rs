use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::exit;
use std::sync::{RwLock, Arc, Mutex};
use std::error::Error;
use std::thread::sleep;
use std::time::Duration;

use libc::__c_anonymous_ptrace_syscall_info_seccomp;

use crate::proxywireprotocol::{JobProfile, JobDesc, CounterSnapshot};

use super::proxy_common::{ProxyErr, unix_ts_us, list_files_with_ext_in};

use super::proxywireprotocol::CounterType;

/***********************
 * PROMETHEUS EXPORTER *
 ***********************/
 struct ExporterEntry {
	name: String,
	ctype: CounterType,
	value: Arc<
				RwLock<
						f64
						>
				>
}

impl ExporterEntry
{
	fn new(name : String, ctype : CounterType) -> ExporterEntry
	{
		ExporterEntry{
			name,
			value : Arc::new(RwLock::new(0.0)),
			ctype
		}
	}
}


struct ExporterEntryGroup {
	basename : String,
	doc: String,
	ht: RwLock<
					HashMap<String, ExporterEntry>
				>
}

impl ExporterEntryGroup
{
	fn new(basename : String, doc : String) -> ExporterEntryGroup
	{
		ExporterEntryGroup{
			basename,
			doc,
			ht : RwLock::new(HashMap::new())
		}
	}

	fn basename(name : String ) -> String
	{
		let spl : Vec<&str> = name.split("{").collect();
		spl[0].to_string()
	}

	fn set(& self, name : &str, value : f64) -> Result<(), ProxyErr>
	{
		match self.ht.write().unwrap().get_mut(name)
		{
			Some(v) => {
				let mut val = v.value.write().unwrap();
				*val = value;
				return Ok(());
			}
			None => {
				return Err(ProxyErr::new("Failed to set counter"));
			}
		}
	}

	fn accumulate(& self, name : &str, value : f64) -> Result<(), ProxyErr>
	{
		match self.ht.write().unwrap().get_mut(name)
		{
			Some(v) => {
				let mut val = v.value.write().unwrap();
				match v.ctype
				{
					CounterType::COUNTER => {
						*val += value;
					},
					CounterType::GAUGE => {
						*val = value;
					}
				}
				return Ok(());
			}
			None => {
				return Err(ProxyErr::new("Failed to set counter"));
			}
		}
	}

	fn push(& self, name : &str, ctype : CounterType) -> Result<(), ProxyErr>
	{
		if self.ht.read().unwrap().contains_key(name)
		{
			return Ok(());
		}
		else
		{
			if name.contains("{")
			{
				if ! name.contains("}")
				{
					return Err(ProxyErr::new(format!("Bad metric name '{}' unmatched brackets",name.to_string()).as_str()));
				}
			}
			let new = ExporterEntry::new(name.to_string(), ctype);
			self.ht.write().unwrap().insert(name.to_string(), new);
		}

		Ok(())
	}

	fn serialize(& self ) -> Result<String, ProxyErr>
	{
		let mut ret: String = String::new();

		ret += format!("# HELP {} {}\n", self.basename, self.doc).as_str();
		ret += format!("# TYPE {} counter\n", self.basename).as_str();

		for (_, exporter_counter) in self.ht.read().unwrap().iter() {
			 // Acquire the Mutex for this specific ExporterEntry
			 let value = exporter_counter.value.read().unwrap();
			 ret += format!("{} {}\n", exporter_counter.name, value).as_str();
		}

		Ok(ret)
	}

	fn snapshot(& self) -> Result<Vec<CounterSnapshot>, ProxyErr>
	{
		let mut ret : Vec<CounterSnapshot> = Vec::new();

		for (_, exporter_counter) in self.ht.read().unwrap().iter() {
			 // Acquire the Mutex for this specific ExporterEntry
			 let value = exporter_counter.value.read().unwrap();
			 ret.push(CounterSnapshot{
				name : exporter_counter.name.to_string(),
				doc : self.doc.to_string(),
				ctype : CounterType::COUNTER,
				value : *value
			 });
		}

		Ok(ret)
	}
}



pub(crate) struct Exporter {
	ht: RwLock<
					HashMap<String, ExporterEntryGroup>
				>
}


impl Exporter {
	pub(crate) fn new() -> Exporter {
		 Exporter {
			  ht: RwLock::new(HashMap::new()),
		 }
	}

	pub(crate) fn accumulate(&self, name: &str, value: f64) -> Result<(), ProxyErr> {
		log::trace!("Exporter accumulate {} {}", name, value);

		let basename = ExporterEntryGroup::basename(name.to_string());

		 if let Some(exporter_counter) = self.ht.read().unwrap().get(basename.as_str())
		 {
			return exporter_counter.accumulate(name, value);
		 }
		 else
		 {
			return Err(ProxyErr::new(format!("No such key {} cannot set it", name).as_str()));
		 }
	}

	pub(crate) fn set(&self, name: &str, value: f64) -> Result<(), ProxyErr> {
		log::trace!("Exporter set {} {}", name, value);

		let basename = ExporterEntryGroup::basename(name.to_string());
		
		if let Some(exporter_counter) = self.ht.read().unwrap().get(basename.as_str())
		{
			return exporter_counter.set(name, value);
		}
		else
		{
		  return Err(ProxyErr::new(format!("No such key {} cannot set it", name).as_str()));
		}
  }

	pub(crate) fn push(&self, name: &str, doc: &str, ctype : CounterType) -> Result<(), ProxyErr> {

		log::trace!("Exporter push {} {} {:?}", name, doc, ctype);

		let basename = ExporterEntryGroup::basename(name.to_string());

		let mut ht = self.ht.write().unwrap();
	
		if let Some(_) = ht.get(basename.as_str())
		{
			return Ok(());
		}
		else
		{
			let ncnt = ExporterEntryGroup::new(basename.to_owned(), doc.to_string());
			ncnt.push(name, ctype)?;
			ht.insert(basename, ncnt);
		 }

		 Ok(())
	}

	pub(crate) fn serialize(&self) -> Result<String, ProxyErr> {
		 let mut ret: String = String::new();

		 for (_, exporter_counter) in self.ht.read().unwrap().iter() {
			  ret += exporter_counter.serialize()?.as_str();
		 }

		 ret += "# EOF\n";

		 Ok(ret)
	}


	pub(crate) fn profile( &self, desc : &JobDesc) -> Result<JobProfile, ProxyErr>
	{
		let mut ret = JobProfile{
			desc : desc.clone(),
			counters : Vec::new()
		};

		for (_, exporter_counter) in self.ht.read().unwrap().iter() {
			let snaps = exporter_counter.snapshot()?;
			ret.counters.extend(snaps);
	  }

		Ok(ret)
	}

}


struct PerJobRefcount
{
	desc : JobDesc,
	counter : i32,
	exporter : Arc<Exporter>
}


impl Drop for PerJobRefcount {
	fn drop(&mut self) {
		log::info!("Dropping per job exporter for {}", self.desc.jobid);
	}
}

impl PerJobRefcount
{
	fn profile(&self) -> Result<JobProfile, ProxyErr>
	{
		self.exporter.profile(&self.desc)
	}
}

pub(crate) struct ExporterFactory
{
	main : Arc<Exporter>,
	perjob : Mutex<
					HashMap<String, 
						PerJobRefcount
					>
				>,
	prefix : PathBuf
}


fn create_dir_or_fail(path : &PathBuf)
{
	if let Err(e) = std::fs::create_dir(&path)
	{
		log::error!("Failed to create directory at {} : {}", path.to_str().unwrap_or(""), e);
		exit(1);
	}
}

impl ExporterFactory
{
	fn check_profile_dir(path : &PathBuf)
	{
		// Main directory
		if ! path.exists()
		{
			create_dir_or_fail(&path);
		}
		else if !path.is_dir()
		{
			log::error!("{} is not a directory cannot use it as per job profile prefix", path.to_str().unwrap_or(""));
			exit(1);
		}

		// Profile subdirectory
		let mut profile_dir = path.clone();
		profile_dir.push("profiles");

		if !profile_dir.exists()
		{
			create_dir_or_fail(&profile_dir);
		}

		// Partial subdirectory
		let mut partial_dir = path.clone();
		partial_dir.push("partial");

		if !partial_dir.exists()
		{
			create_dir_or_fail(&partial_dir);
		}


	}

	fn profile_parse_jobid(target : & String) -> Result<String, Box<dyn Error>>
	{
		let path = PathBuf::from(target);
		let filename = path.file_name().ok_or("Failed to parse path")?.to_string_lossy().to_string();

		if let Some(jobid) = filename.split("___").next()
		{
			return Ok(jobid.to_string());
		}

		Err(ProxyErr::newboxed("Failed to parse jobid"))
	}

	fn accumulate_a_profile(profile_dir : & PathBuf , target : & String) -> Result<(), Box<dyn Error>>
	{
		let file = fs::File::open(&target)?;
		let mut content : JobProfile = serde_json::from_reader(file)?;

		/* Compute path to profile for given job  */
		let jobid = ExporterFactory::profile_parse_jobid(target)?;
		let mut target_prof = profile_dir.clone();
		target_prof.push(format!("{}.profile", jobid));

		if target_prof.is_file()
		{
			/* We need to load and accumulate the existing profile */
			let e_profile_file = fs::File::open(&target_prof)?;
			let existing_prof : JobProfile = serde_json::from_reader(e_profile_file)?;
			/* Aggregate the existing content */
			content.merge(existing_prof)?;
		}


		/* Overwrite the profile */
		let outfile = fs::File::create(target_prof)?;
		serde_json::to_writer(outfile, &content)?;

		/* If we are here we managed to read and collect the file */
		fs::remove_file(target).ok();

		Ok(())
	}



	fn aggregate_profiles(prefix : PathBuf) -> Result<(), Box<dyn Error>>
	{
		let mut profile_dir = prefix.clone();
		profile_dir.push("profiles");

		let mut partial_dir = prefix.clone();
		partial_dir.push("partial");

		assert!(profile_dir.is_dir());
		assert!(partial_dir.is_dir());

		loop
		{
			let ret = list_files_with_ext_in(&partial_dir, ".partialprofile")?;

			for partial in ret.iter()
			{
				if let Err(e) = ExporterFactory::accumulate_a_profile(&profile_dir, partial)
				{
					log::error!("Failed to process {} : {}", partial, e.to_string());
				}
				else
				{
					log::trace!("Aggregated profile {}", partial);
				}
			}

			sleep(Duration::from_secs(1));
		}
	}



	pub(crate) fn new(profile_prefix : PathBuf, aggregate : bool) -> Arc<ExporterFactory>
	{
		ExporterFactory::check_profile_dir(&profile_prefix);

		if aggregate == true
		{
			let thread_prefix = profile_prefix.clone();
			// Start Aggreg thread
			std::thread::spawn(move ||{
				ExporterFactory::aggregate_profiles(thread_prefix).unwrap();
			});
		}

		Arc::new(ExporterFactory{
			main: Arc::new(Exporter::new()),
			perjob: Mutex::new(HashMap::new()),
			prefix : profile_prefix
		})
	}

	pub(crate) fn get_main(&self) -> Arc<Exporter>
	{
		return self.main.clone();
	}

	pub(crate) fn resolve_by_id(&self, jobid : &String) -> Option<Arc<Exporter>>
	{
		if let Some(r) = self.perjob.lock().unwrap().get(jobid)
		{
			return Some(r.exporter.clone());
		}
		None
	}

	pub(crate) fn resolve_job(& self, desc : & JobDesc) -> Arc<Exporter>
	{

		let mut ht: std::sync::MutexGuard<'_, HashMap<String, PerJobRefcount>> = self.perjob.lock().unwrap();

		let v = match ht.get_mut(&desc.jobid)
		{
			Some(e) => {
				log::info!("Cloning existing job exporter for {}", &desc.jobid);
				/* Incr Refcount */
				e.counter += 1;
				log::debug!("ACQUIRING Per Job exporter {} has refcount {}", &desc.jobid, e.counter);
				e.exporter.clone()
			},
			None => {
				log::info!("Creating new job exporter for {}", &desc.jobid);
				let new = PerJobRefcount{
						desc : desc.clone(),
						exporter : Arc::new(Exporter::new()),
						counter : 1
				};
				let ret = new.exporter.clone();
				ht.insert(desc.jobid.to_string(), new);
				ret
			}
		};

		return v;
	}

	fn saveprofile(&self, per_job : & PerJobRefcount, desc : &JobDesc) -> Result<(), Box<dyn Error>>
	{
		let snap = per_job.exporter.profile(desc)?;

		let mut target_dir = self.prefix.clone();
		target_dir.push("partial");

		let host = gethostname::gethostname();
		let hostname = host.to_str().unwrap_or("unknown");

		let fname = format!("{}___{}.{}.partialprofile",  desc.jobid, hostname, unix_ts_us());

		target_dir.push(fname);

		log::info!("Saving partial profile to {}", target_dir.to_str().unwrap_or(""));

		let file = fs::File::create(target_dir)?;

		serde_json::to_writer(file, &snap)?;

		Ok(())
	}

	pub(crate) fn list_jobs(&self) -> Vec<JobDesc>
	{
		self.perjob.lock().unwrap().values().map(|k| k.desc.clone()).collect()
	}

	pub(crate) fn profiles(&self) -> Vec<JobProfile>
	{
		let mut ret : Vec<JobProfile> = Vec::new();

		if let Ok(ht) = self.perjob.lock()
		{
			for v in ht.values()
			{
				if let Ok(p) = v.profile()
				{
					ret.push(p);
				}
			}
		}

		ret
	}


	pub(crate) fn profile_of(&self, jobid : & String) -> Result<JobProfile, ProxyErr>
	{
		if let Some(elem) = self.perjob.lock().unwrap().get(jobid)
		{
			return elem.profile();
		}

		Err(ProxyErr::new("No such Job ID"))
	}


	pub(crate) fn relax_job(&self,  desc : &JobDesc) -> Result<(), Box<dyn Error>>
	{
		let mut ht: std::sync::MutexGuard<'_, HashMap<String, PerJobRefcount>> = self.perjob.lock().unwrap();

		if let Some(job_entry) = ht.get_mut(&desc.jobid)
		{
			job_entry.counter -= 1;
			log::debug!("RELAXING Per Job exporter {} has refcount {}", desc.jobid, job_entry.counter);
			assert!(0 <= job_entry.counter);
			if job_entry.counter == 0
			{
				/* Serialize */
				if let Some(perjob) = ht.get(&desc.jobid)
				{
					self.saveprofile(perjob, desc)?;
					/* Delete */
					ht.remove(&desc.jobid);
				}
			}
		}
		else
		{
			return Err(ProxyErr::newboxed("No such job to remove"));
		}

		Ok(())
	}


}