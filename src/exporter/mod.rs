use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::exit;
use std::sync::{RwLock, Arc, Mutex};
use std::error::Error;

use crate::proxywireprotocol::{JobProfile, JobDesc, CounterSnapshot};

use super::proxy_common::{ProxyErr, unix_ts_us};

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

	fn profile( &self, desc : &JobDesc) -> Result<JobProfile, ProxyErr>
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
	job : String,
	counter : i32,
	exporter : Arc<Exporter>
}


impl Drop for PerJobRefcount {
	fn drop(&mut self) {
		log::info!("Dropping per job exporter for {}", self.job);
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
	prefix : PathBuf,
	aggregate : bool
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

	pub(crate) fn new(profile_prefix : PathBuf, aggregate : bool) -> Arc<ExporterFactory>
	{
		ExporterFactory::check_profile_dir(&profile_prefix);

		if aggregate == true
		{
			// Start Aggreg thread
		}

		Arc::new(ExporterFactory{
			main: Arc::new(Exporter::new()),
			perjob: Mutex::new(HashMap::new()),
			prefix : profile_prefix,
			aggregate
		})
	}

	pub(crate) fn get_main(&self) -> Arc<Exporter>
	{
		return self.main.clone();
	}

	pub(crate) fn resolve_job(& self, jobid : & String) -> Arc<Exporter>
	{

		let mut ht: std::sync::MutexGuard<'_, HashMap<String, PerJobRefcount>> = self.perjob.lock().unwrap();

		let v = match ht.get_mut(jobid)
		{
			Some(e) => {
				log::info!("Cloning existing job exporter for {}", jobid);
				/* Incr Refcount */
				e.counter += 1;
				log::debug!("ACQUIRING Per Job exporter {} has refcount {}", jobid, e.counter);
				e.exporter.clone()
			},
			None => {
				log::info!("Creating new job exporter for {}", jobid);
				let new = PerJobRefcount{
						job : jobid.to_string(),
						exporter : Arc::new(Exporter::new()),
						counter : 1
				};
				let ret = new.exporter.clone();
				ht.insert(jobid.to_string(), new);
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