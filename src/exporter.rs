use retry::{delay::Fixed, retry};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::sleep;
use std::time::Duration;

use crate::proxywireprotocol::{
    ApiResponse, CounterSnapshot, CounterType, JobDesc, JobProfile, ValueAlarm, ValueAlarmTrigger,
};

use super::proxy_common::{hostname, list_files_with_ext_in, unix_ts_us, ProxyErr};

use crate::scrapper::{ProxyScraper, ProxyScraperSnapshot};

/***********************
 * PROMETHEUS EXPORTER *
 ***********************/

struct ExporterEntry {
    value: Arc<RwLock<CounterSnapshot>>,
}

impl ExporterEntry {
    fn new(value: CounterSnapshot) -> ExporterEntry {
        ExporterEntry {
            value: Arc::new(RwLock::new(value)),
        }
    }
}

struct ExporterEntryGroup {
    basename: String,
    doc: String,
    ht: RwLock<HashMap<String, ExporterEntry>>,
}

impl ExporterEntryGroup {
    fn new(basename: String, doc: String) -> ExporterEntryGroup {
        ExporterEntryGroup {
            basename,
            doc,
            ht: RwLock::new(HashMap::new()),
        }
    }

    fn basename(name: String) -> String {
        let spl: Vec<&str> = name.split('{').collect();
        spl[0].to_string()
    }

    fn set(&self, value: CounterSnapshot) -> Result<(), ProxyErr> {
        match self.ht.write().unwrap().get_mut(&value.name) {
            Some(v) => {
                let mut val = v.value.write().unwrap();
                *val = value;
                Ok(())
            }
            None => Err(ProxyErr::new("Failed to set counter")),
        }
    }

    fn accumulate(&self, snapshot: &CounterSnapshot, merge: bool) -> Result<(), ProxyErr> {
        match self.ht.write().unwrap().get_mut(&snapshot.name) {
            Some(v) => {
                let mut val = v.value.write().unwrap();
                if merge {
                    val.merge(snapshot)?;
                } else {
                    val.set(snapshot)?;
                }
                Ok(())
            }
            None => Err(ProxyErr::new(
                format!("Failed to accumulate {} {:?}", snapshot.name, snapshot).as_str(),
            )),
        }
    }

    fn get(&self, metric: &String) -> Result<Arc<RwLock<CounterSnapshot>>, ProxyErr> {
        let ret = self
            .ht
            .read()
            .unwrap()
            .get(metric)
            .ok_or(ProxyErr::new("Failed to get in metric group"))?
            .value
            .clone();

        Ok(ret)
    }

    fn push(&self, snapshot: CounterSnapshot) -> Result<(), ProxyErr> {
        let name = snapshot.name.to_string();
        if self.ht.read().unwrap().contains_key(&name) {
            return Ok(());
        } else {
            if name.contains('{') && !name.contains('}') {
                return Err(ProxyErr::new(
                    format!("Bad metric name '{}' unmatched brackets", name).as_str(),
                ));
            }
            let new = ExporterEntry::new(snapshot);
            self.ht.write().unwrap().insert(name, new);
        }

        Ok(())
    }

    fn serialize(&self) -> Result<String, ProxyErr> {
        let mut ret: String = String::new();

        ret += format!("# HELP {} {}\n", self.basename, self.doc).as_str();
        ret += format!("# TYPE {} counter\n", self.basename).as_str();

        for (_, exporter_counter) in self.ht.read().unwrap().iter() {
            // Acquire the Mutex for this specific ExporterEntry
            let value = exporter_counter.value.read().unwrap();
            ret += value.serialize().as_str();
        }

        Ok(ret)
    }

    fn snapshot(&self) -> Result<Vec<CounterSnapshot>, ProxyErr> {
        let mut ret: Vec<CounterSnapshot> = Vec::new();

        for (_, exporter_counter) in self.ht.read().unwrap().iter() {
            // Acquire the Mutex for this specific ExporterEntry
            let value = exporter_counter.value.read().unwrap();
            ret.push(value.clone());
        }

        Ok(ret)
    }
}

pub(crate) struct Exporter {
    ht: RwLock<HashMap<String, ExporterEntryGroup>>,
    alarms: RwLock<HashMap<String, ValueAlarm>>,
}

impl Exporter {
    pub(crate) fn new() -> Exporter {
        Exporter {
            ht: RwLock::new(HashMap::new()),
            alarms: RwLock::new(HashMap::new()),
        }
    }

    pub(crate) fn accumulate(&self, value: &CounterSnapshot, merge: bool) -> Result<(), ProxyErr> {
        let basename = ExporterEntryGroup::basename(value.name.to_string());

        if let Some(exporter_counter) = self.ht.read().unwrap().get(basename.as_str()) {
            exporter_counter.accumulate(value, merge)
        } else {
            Err(ProxyErr::new(format!(
                "No such key {} cannot set it",
                value.name
            )))
        }
    }

    pub(crate) fn get(&self, metric: &String) -> Result<Arc<RwLock<CounterSnapshot>>, ProxyErr> {
        let basename = ExporterEntryGroup::basename(metric.to_string());

        if let Some(exporter_counter) = self.ht.read().unwrap().get(basename.as_str()) {
            exporter_counter.get(metric)
        } else {
            Err(ProxyErr::new(format!(
                "No such key {} cannot get it",
                metric
            )))
        }
    }

    pub(crate) fn set(&self, value: CounterSnapshot) -> Result<(), ProxyErr> {
        log::trace!("Exporter set {} {:?}", value.name, value);

        let basename = ExporterEntryGroup::basename(value.name.to_string());

        if let Some(exporter_counter) = self.ht.read().unwrap().get(basename.as_str()) {
            exporter_counter.set(value)
        } else {
            return Err(ProxyErr::new(
                format!("No such key {} cannot set it", value.name).as_str(),
            ));
        }
    }

    pub(crate) fn push(&self, value: &CounterSnapshot) -> Result<(), ProxyErr> {
        log::trace!("Exporter push {:?}", value);

        let basename = ExporterEntryGroup::basename(value.name.to_string());

        let mut ht = self.ht.write().unwrap();

        if let Some(ncnt) = ht.get(basename.as_str()) {
            ncnt.push(value.clone())?;
            return Ok(());
        } else {
            let ncnt = ExporterEntryGroup::new(basename.to_owned(), value.doc.to_string());
            ncnt.push(value.clone())?;
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

    pub(crate) fn profile(&self, desc: &JobDesc) -> Result<JobProfile, ProxyErr> {
        let mut ret = JobProfile {
            desc: desc.clone(),
            counters: Vec::new(),
        };

        for (_, exporter_counter) in self.ht.read().unwrap().iter() {
            let snaps = exporter_counter.snapshot()?;
            ret.counters.extend(snaps);
        }

        Ok(ret)
    }

    pub(crate) fn add_alarm(
        &self,
        name: String,
        metric: String,
        op: String,
        value: f64,
    ) -> Result<(), ProxyErr> {
        let cnt: Arc<RwLock<CounterSnapshot>> = self.get(&metric)?;
        let alarm = ValueAlarm::new(&name, cnt, op, value)?;

        log::info!("Adding new alarm {}", alarm);

        let mut lht = self.alarms.write().unwrap();

        if lht.contains_key(&name) {
            return Err(ProxyErr::new(format!("Alarm {} is already defined", name)));
        }

        lht.insert(name, alarm);

        Ok(())
    }

    pub(crate) fn delete_alarm(&self, alarm_name: &String) -> Result<(), ProxyErr> {
        self.alarms
            .write()
            .unwrap()
            .remove(alarm_name)
            .ok_or(ProxyErr::new(format!(
                "Failed to remove alarm {}",
                alarm_name
            )))?;
        Ok(())
    }

    pub(crate) fn check_alarms(&self) -> Vec<ValueAlarmTrigger> {
        let alarmv = self.alarms.read().unwrap();

        let mut ret: Vec<ValueAlarmTrigger> = Vec::new();

        for (_, a) in alarmv.iter() {
            if let Some(v) = a.check() {
                ret.push(v);
            }
        }

        ret
    }
}

struct PerJobRefcount {
    desc: JobDesc,
    counter: i32,
    exporter: Arc<Exporter>,
    saveprofile: bool,
}

impl Drop for PerJobRefcount {
    fn drop(&mut self) {
        log::debug!("Dropping per job exporter for {}", self.desc.jobid);
    }
}

impl PerJobRefcount {
    fn profile(&self) -> Result<JobProfile, ProxyErr> {
        self.exporter.profile(&self.desc)
    }
}

pub(crate) struct ExporterFactory {
    main: Arc<Exporter>,
    pernode: Arc<Exporter>,
    perjob: Mutex<HashMap<String, PerJobRefcount>>,
    prefix: PathBuf,
    scrapes: Mutex<HashMap<String, ProxyScraper>>,
}

fn create_dir_or_fail(path: &PathBuf) {
    if let Err(e) = std::fs::create_dir(path) {
        log::error!(
            "Failed to create directory at {} : {}",
            path.to_str().unwrap_or(""),
            e
        );
        exit(1);
    }
}

impl ExporterFactory {
    fn check_profile_dir(path: &PathBuf) {
        // Main directory
        if !path.exists() {
            create_dir_or_fail(path);
        } else if !path.is_dir() {
            log::error!(
                "{} is not a directory cannot use it as per job profile prefix",
                path.to_str().unwrap_or("")
            );
            exit(1);
        }

        // Profile subdirectory
        let mut profile_dir = path.clone();
        profile_dir.push("profiles");

        if !profile_dir.exists() {
            create_dir_or_fail(&profile_dir);
        }

        // Partial subdirectory
        let mut partial_dir = path.clone();
        partial_dir.push("partial");

        if !partial_dir.exists() {
            create_dir_or_fail(&partial_dir);
        }
    }

    fn profile_parse_jobid(target: &String) -> Result<String, Box<dyn Error>> {
        let path = PathBuf::from(target);
        let filename = path
            .file_name()
            .ok_or("Failed to parse path")?
            .to_string_lossy()
            .to_string();

        if let Some(jobid) = filename.split("___").next() {
            return Ok(jobid.to_string());
        }

        Err(ProxyErr::newboxed("Failed to parse jobid"))
    }

    fn accumulate_a_profile(profile_dir: &Path, target: &String) -> Result<(), Box<dyn Error>> {
        let file = fs::File::open(target)?;
        let mut content: JobProfile = serde_json::from_reader(file)?;

        /* Compute path to profile for given job  */
        let jobid = ExporterFactory::profile_parse_jobid(target)?;
        let mut target_prof = profile_dir.to_path_buf();
        target_prof.push(format!("{}.profile", jobid));

        if target_prof.is_file() {
            /* We need to load and accumulate the existing profile */
            let e_profile_file = fs::File::open(&target_prof)?;
            let existing_prof: JobProfile = serde_json::from_reader(e_profile_file)?;
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

    fn aggregate_profiles(prefix: PathBuf) -> Result<(), Box<dyn Error>> {
        let mut profile_dir = prefix.clone();
        profile_dir.push("profiles");

        let mut partial_dir = prefix.clone();
        partial_dir.push("partial");

        assert!(profile_dir.is_dir());
        assert!(partial_dir.is_dir());

        loop {
            let ret = list_files_with_ext_in(&partial_dir, ".partialprofile")?;

            for partial in ret.iter() {
                if let Err(e) = ExporterFactory::accumulate_a_profile(&profile_dir, partial) {
                    log::error!("Failed to process {} : {}", partial, e.to_string());
                } else {
                    log::trace!("Aggregated profile {}", partial);
                }
            }

            sleep(Duration::from_secs(1));
        }
    }

    fn run_scrapping(&self) {
        loop {
            let mut to_delete: Vec<String> = Vec::new();

            /* Scrape all the candidates */
            for (k, v) in self.scrapes.lock().unwrap().iter_mut() {
                if let Err(e) = v.scrape() {
                    log::error!("Failed to scrape {} : {}", k, e);
                    to_delete.push(k.to_string());
                }
            }

            /* Remove failed scrapes */
            for k in to_delete {
                self.scrapes.lock().unwrap().remove(&k);
            }

            sleep(Duration::from_secs(1));
        }
    }

    pub(crate) fn add_scrape(
        factory: Arc<ExporterFactory>,
        url: &String,
        period: u64,
    ) -> Result<(), Box<dyn Error>> {
        let new = ProxyScraper::new(url, period, factory.clone())?;
        factory
            .scrapes
            .lock()
            .unwrap()
            .insert(new.url().to_string(), new);
        Ok(())
    }

    pub(crate) fn list_scrapes(&self) -> Vec<ProxyScraperSnapshot> {
        let ret: Vec<ProxyScraperSnapshot> = self
            .scrapes
            .lock()
            .unwrap()
            .iter()
            .map(|(_, v)| v.snapshot())
            .collect();
        ret
    }

    pub(crate) fn join(
        root_server: &String,
        my_server_address: &String,
        period: u64,
    ) -> Result<(), ProxyErr> {
        let pivot_url = root_server.to_string() + "/pivot?from=" + my_server_address;

        /* We add some delay as the root server may get smashed */
        let resp = retry(Fixed::from_millis(2000).take(5), || {
            ApiResponse::query(&pivot_url)
        })?;

        let target_url = "http://".to_string()
            + resp.operation.as_str()
            + "/join?to="
            + my_server_address
            + "&period="
            + period.to_string().as_str();

        /* We add some delay as the root server may get smashed */
        match ApiResponse::query(&target_url) {
            Ok(_) => {
                log::info!(
                    "Joining aggregating proxy {} with period {}",
                    root_server,
                    period
                );
                Ok(())
            }
            Err(e) => Err(ProxyErr::from(e)),
        }
    }

    pub(crate) fn new(profile_prefix: PathBuf, aggregate: bool) -> Arc<ExporterFactory> {
        ExporterFactory::check_profile_dir(&profile_prefix);

        if aggregate {
            let thread_prefix = profile_prefix.clone();
            // Start Aggreg thread
            std::thread::spawn(move || {
                ExporterFactory::aggregate_profiles(thread_prefix).unwrap();
            });
        }

        let ret = Arc::new(ExporterFactory {
            main: Arc::new(Exporter::new()),
            pernode: Arc::new(Exporter::new()),
            perjob: Mutex::new(HashMap::new()),
            prefix: profile_prefix,
            scrapes: Mutex::new(HashMap::new()),
        });

        let scrape_ref = ret.clone();
        // Start Scraping thread
        std::thread::spawn(move || {
            scrape_ref.run_scrapping();
        });

        /* This creates a job entry for the cumulative job */
        let main_job = PerJobRefcount {
            desc: JobDesc {
                jobid: "main".to_string(),
                command: "Sum of all Jobs".to_string(),
                size: 0,
                nodelist: "".to_string(),
                partition: "".to_string(),
                cluster: "".to_string(),
                run_dir: "".to_string(),
                start_time: 0,
                end_time: 0,
            },
            exporter: ret.main.clone(),
            counter: 1,
            saveprofile: false,
        };
        ret.perjob
            .lock()
            .unwrap()
            .insert(main_job.desc.jobid.to_string(), main_job);

        /* This creates a job entry for the pernode job */
        let node_job = PerJobRefcount {
            desc: JobDesc {
                jobid: format!("Node: {}", hostname()),
                command: format!("Sum of all Jobs running on {}", hostname()),
                size: 0,
                nodelist: hostname(),
                partition: "".to_string(),
                cluster: "".to_string(),
                run_dir: "".to_string(),
                start_time: 0,
                end_time: 0,
            },
            exporter: ret.pernode.clone(),
            counter: 1,
            saveprofile: false,
        };
        ret.perjob
            .lock()
            .unwrap()
            .insert(node_job.desc.jobid.to_string(), node_job);

        /* Now insert the default system scrape */
        let systemurl = "/system".to_string();
        if let Ok(sys_metrics) = ProxyScraper::new(&systemurl, 5, ret.clone()) {
            ret.scrapes.lock().unwrap().insert(systemurl, sys_metrics);
        }

        ret
    }

    pub(crate) fn get_main(&self) -> Arc<Exporter> {
        self.main.clone()
    }

    pub(crate) fn get_node(&self) -> Arc<Exporter> {
        self.pernode.clone()
    }

    pub(crate) fn resolve_by_id(&self, jobid: &String) -> Option<Arc<Exporter>> {
        if let Some(r) = self.perjob.lock().unwrap().get(jobid) {
            return Some(r.exporter.clone());
        }
        None
    }

    pub(crate) fn resolve_job(&self, desc: &JobDesc, tobesaved: bool) -> Arc<Exporter> {
        let mut ht: std::sync::MutexGuard<'_, HashMap<String, PerJobRefcount>> =
            self.perjob.lock().unwrap();

        let v = match ht.get_mut(&desc.jobid) {
            Some(e) => {
                log::debug!("Cloning existing job exporter for {}", &desc.jobid);
                /* Incr Refcount */
                e.counter += 1;
                /* Make sure save flags match */
                if tobesaved {
                    e.saveprofile = true;
                }
                log::debug!(
                    "ACQUIRING Per Job exporter {} has refcount {}",
                    &desc.jobid,
                    e.counter
                );
                e.exporter.clone()
            }
            None => {
                log::debug!("Creating new job exporter for {}", &desc.jobid);
                let new = PerJobRefcount {
                    desc: desc.clone(),
                    exporter: Arc::new(Exporter::new()),
                    counter: 1,
                    saveprofile: tobesaved,
                };
                let ret = new.exporter.clone();
                ht.insert(desc.jobid.to_string(), new);
                ret
            }
        };

        v
    }

    fn saveprofile(&self, per_job: &PerJobRefcount, desc: &JobDesc) -> Result<(), Box<dyn Error>> {
        let snap = per_job.exporter.profile(desc)?;

        let mut target_dir = self.prefix.clone();
        target_dir.push("partial");

        let hostname = hostname();

        let fname = format!(
            "{}___{}.{}.{}.partialprofile",
            desc.jobid,
            hostname,
            std::process::id(),
            unix_ts_us()
        );

        target_dir.push(fname);

        log::debug!(
            "Saving partial profile to {}",
            target_dir.to_str().unwrap_or("")
        );

        let file = fs::File::create(target_dir)?;

        serde_json::to_writer(file, &snap)?;

        Ok(())
    }

    pub(crate) fn list_jobs(&self) -> Vec<JobDesc> {
        self.perjob
            .lock()
            .unwrap()
            .values()
            .map(|k| k.desc.clone())
            .collect()
    }

    pub(crate) fn profiles(&self) -> Vec<JobProfile> {
        let mut ret: Vec<JobProfile> = Vec::new();

        if let Ok(ht) = self.perjob.lock() {
            for v in ht.values() {
                if let Ok(p) = v.profile() {
                    ret.push(p);
                }
            }
        }

        ret
    }

    pub(crate) fn profile_of(&self, jobid: &String) -> Result<JobProfile, ProxyErr> {
        if let Some(elem) = self.perjob.lock().unwrap().get(jobid) {
            return elem.profile();
        }

        Err(ProxyErr::new("No such Job ID"))
    }

    pub(crate) fn relax_job(&self, desc: &JobDesc) -> Result<(), Box<dyn Error>> {
        let mut ht: std::sync::MutexGuard<'_, HashMap<String, PerJobRefcount>> =
            self.perjob.lock().unwrap();

        if let Some(job_entry) = ht.get_mut(&desc.jobid) {
            job_entry.counter -= 1;
            log::debug!(
                "RELAXING Per Job exporter {} has refcount {}",
                desc.jobid,
                job_entry.counter
            );
            assert!(0 <= job_entry.counter);
            if job_entry.counter == 0 {
                /* Serialize */
                if let Some(perjob) = ht.get(&desc.jobid) {
                    self.saveprofile(perjob, desc)?;
                    /* Delete */
                    ht.remove(&desc.jobid);
                }
            }
        } else {
            return Err(ProxyErr::newboxed("No such job to remove"));
        }

        Ok(())
    }

    pub(crate) fn push(
        &self,
        name: &str,
        doc: &str,
        ctype: CounterType,
        perjob_exporter: Option<Arc<Exporter>>,
    ) -> Result<(), ProxyErr> {
        let snapshot = CounterSnapshot {
            name: name.to_string(),
            doc: doc.to_string(),
            ctype,
        };
        self.get_main().push(&snapshot)?;
        self.get_node().push(&snapshot)?;

        if let Some(e) = perjob_exporter {
            e.push(&snapshot)?;
        }

        Ok(())
    }

    pub(crate) fn accumulate(
        &self,
        name: &str,
        ctype: CounterType,
        perjob_exporter: Option<Arc<Exporter>>,
    ) -> Result<(), ProxyErr> {
        let snapshot = CounterSnapshot {
            name: name.to_string(),
            doc: "".to_string(),
            ctype,
        };

        self.get_main().accumulate(&snapshot, false)?;
        self.get_node().accumulate(&snapshot, false)?;

        if let Some(e) = perjob_exporter {
            e.accumulate(&snapshot, false)?;
        }

        Ok(())
    }

    pub(crate) fn add_alarm(
        &self,
        name: String,
        target_job: String,
        metric: String,
        op: String,
        value: f64,
    ) -> Result<(), ProxyErr> {
        let perjobht = self.perjob.lock().unwrap();

        let perjob = perjobht.get(&target_job).ok_or(ProxyErr::new(format!(
            "Failed to locate job {}",
            target_job
        )))?;

        perjob.exporter.add_alarm(name, metric, op, value)?;

        Ok(())
    }

    pub(crate) fn check_alarms(&self) -> HashMap<String, Vec<ValueAlarmTrigger>> {
        let mut ret: HashMap<String, Vec<ValueAlarmTrigger>> = HashMap::new();

        let perjobht = self.perjob.lock().unwrap();

        for (k, v) in perjobht.iter() {
            let alarms: Vec<ValueAlarmTrigger> = v.exporter.check_alarms();
            ret.insert(k.to_string(), alarms);
        }

        ret
    }

    pub(crate) fn list_alarms(&self) -> HashMap<String, Vec<ValueAlarmTrigger>> {
        let mut ret: HashMap<String, Vec<ValueAlarmTrigger>> = HashMap::new();

        let perjobht = self.perjob.lock().unwrap();

        for (k, v) in perjobht.iter() {
            let alarms: Vec<ValueAlarmTrigger> = v
                .exporter
                .alarms
                .read()
                .unwrap()
                .iter()
                .map(|(_, v)| v.as_trigger(None))
                .collect();
            ret.insert(k.to_string(), alarms);
        }

        ret
    }

    pub(crate) fn get_local_job_exporters(&self) -> Vec<Arc<Exporter>> {
        self.perjob
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, v)| v.saveprofile)
            .map(|(_, v)| v.exporter.clone())
            .collect()
    }

    pub(crate) fn delete_alarm(
        &self,
        target_job: &String,
        alarm_name: &String,
    ) -> Result<(), ProxyErr> {
        let perjobht = self.perjob.lock().unwrap();

        let perjob = perjobht.get(target_job).ok_or(ProxyErr::new(format!(
            "Failed to locate job {}",
            target_job
        )))?;

        perjob.exporter.delete_alarm(alarm_name)?;

        Ok(())
    }
}
