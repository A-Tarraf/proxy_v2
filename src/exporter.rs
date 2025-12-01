use retry::{delay::Fixed, retry};
use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::sleep;
use std::time::Duration;

use crate::proxy_common;
use crate::proxywireprotocol::{
    ApiResponse, CounterSnapshot, CounterType, JobDesc, JobProfile, ValueAlarm, ValueAlarmTrigger,
};

use crate::profiles::ProfileView;
use crate::trace::{Trace, TraceView};

use super::proxy_common::{hostname, ProxyErr};

use crate::ftio::FtioClient;

use crate::scrapper::{ProxyScraper, ProxyScraperSnapshot};

/***********************
 * PROMETHEUS EXPORTER *
 ***********************/

/// This is a refcounted reference to a counter and
/// its documentation this allows to lock at counter
/// granularity if needed
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

/// This is a group of values used to have counters with the
/// same prefix stored in the same list. This is important when generating
/// the prometheus output as the format requires counters of the
/// same prefix to be listed with only one TYPE header for example:
///
/// ```
/// # HELP proxy_network_receive_packets_error_total Total number of erroneous  packets received on the given device
/// # TYPE proxy_network_receive_packets_error_total counter
/// proxy_network_receive_packets_error_total{interface="enp3s0"} 0
/// proxy_network_receive_packets_error_total{interface="lo"} 0
/// proxy_network_receive_packets_error_total{interface="docker0"} 0
/// ```
///
/// The basename is the prefix here : proxy_network_receive_packets_error_total
struct ExporterEntryGroup {
    /// Common basename
    basename: String,
    /// Common documentation
    doc: String,
    /// List of values (stored with their full name including the {XXX})
    ht: RwLock<HashMap<String, ExporterEntry>>,
}

impl ExporterEntryGroup {
    /// Create a new ExporterEntryGroup
    fn new(basename: String, doc: String) -> ExporterEntryGroup {
        ExporterEntryGroup {
            basename,
            doc,
            ht: RwLock::new(HashMap::new()),
        }
    }

    /// Get the basename of the ExporterEntryGroup
    fn basename(name: String) -> String {
        let spl: Vec<&str> = name.split('{').collect();
        spl[0].to_string()
    }

    #[allow(unused)]
    /// Set a value in the ExporterEntryGroup
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

    /// Accumulate a value
    ///
    /// This will sum up data
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

    /// Get a reference to a value
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

    /// Insert a new value in the counter list
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

    #[allow(unused)]
    /// Generate the prometheus data from the couter list
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

    /// Clone the current the counter list as a vector of CounterSnapshot
    fn snapshot(&self, full: bool) -> Result<Vec<CounterSnapshot>, ProxyErr> {
        let mut ret: Vec<CounterSnapshot> = Vec::new();

        for (_, exporter_counter) in self.ht.read().unwrap().iter() {
            // Acquire the Mutex for this specific ExporterEntry
            let value = exporter_counter.value.read().unwrap().clone();
            if value.hasdata() || full {
                ret.push(value.clone());
            }
        }

        Ok(ret)
    }
}

/// An exporter is the central metric storage structure
/// It holds a hashmap of ExporterEntryGroup which themselves
/// store the various counter values.
///
/// It is also host for the alarms which are applied to the
/// various metrics using the `check_alarms` call.
pub(crate) struct Exporter {
    /// List of metrics stored by basename in ExporterEntryGroup
    ht: RwLock<HashMap<String, ExporterEntryGroup>>,
    /// List of alarms each refering to a counter
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

    #[allow(unused)]
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

    #[allow(unused)]
    pub(crate) fn serialize(&self) -> Result<String, ProxyErr> {
        let mut ret: String = String::new();

        for (_, exporter_counter) in self.ht.read().unwrap().iter() {
            ret += exporter_counter.serialize()?.as_str();
        }

        ret += "# EOF\n";

        Ok(ret)
    }

    pub(crate) fn profile(&self, desc: &JobDesc, full: bool) -> Result<JobProfile, ProxyErr> {
        let mut ret = JobProfile {
            desc: desc.clone(),
            counters: Vec::new(),
        };

        for (_, exporter_counter) in self.ht.read().unwrap().iter() {
            let snaps = exporter_counter.snapshot(full)?;
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

/// This structure is used to manage the job refcounting
/// It creates an exporter for each new job and keeps
/// track of the number of references onto itself
struct PerJobRefcount {
    /// Description of the job as retrieved from remote
    desc: JobDesc,
    /// Number of references to the job
    counter: i32,
    /// Exporter storing job counters
    exporter: Arc<Exporter>,
    /// This is true only when the job is local (connected from proxy.rs)
    /// A job from a scrapper is not a local one
    /// It is used to only blame node-local metrics to local jobs
    islocal: bool,
}

impl Drop for PerJobRefcount {
    fn drop(&mut self) {
        log::debug!("Dropping per job exporter for {}", self.desc.jobid);
    }
}

impl PerJobRefcount {
    fn profile(&self, full: bool) -> Result<JobProfile, ProxyErr> {
        self.exporter.profile(&self.desc, full)
    }
}

/// This is the central pivot for metric and job management
/// in the metric proxy all operations pass trough here
/// and they are then dispatched to individual exporter instances

pub(crate) struct ExporterFactory {
    /// The main exporter summing contributions
    /// from all others
    main: Arc<Exporter>,
    /// The pernode exporter storing all node-local
    /// Contributions
    pernode: Arc<Exporter>,
    /// An hashtable on each PerJob instance
    /// Each of those instance contains the
    /// corresponding exporter
    perjob: Mutex<HashMap<String, PerJobRefcount>>,
    /// List of scrapres to be run a dedicated thread
    /// will run them according to their polling
    /// frequency
    scrapes: Mutex<HashMap<String, ProxyScraper>>,
    /// Pending scrapes to be backpushed
    pending_scrapes: Mutex<Vec<(String, ProxyScraper)>>,
    /// Instance of the profile manager
    /// in charge of listing and loading profiles
    pub profile_store: Arc<ProfileView>,
    /// Sets this Proxy as aggregator meaning that it
    /// is in charge of storing profiles
    /// the -i option to the proxy sets this to false
    aggregator: bool,
    /// Max trace size defines
    max_trace_size: usize,
    /// This is where the traces are stored
    pub trace_store: Arc<TraceView>,
    /// Client to FTIO server
    pub ftio_client: Arc<FtioClient>,
    pub root_proxy: Arc<RwLock<Option<String>>>,
    pub web_url: Arc<RwLock<Option<String>>>,
    pub period: Arc<RwLock<u64>>,
    pub branches: u64,
}

impl ExporterFactory {
    /// This function if the mainloop of the scrapting thread
    /// It runs infinitely every 1 second checking all scrapes
    fn run_scrapping(&self) {
        loop {
            let mut to_delete: Vec<String> = Vec::new();

            /* Scrape all the candidates */
            if let Ok(scrapes) = self.scrapes.lock().as_mut() {
                for (k, v) in scrapes.iter_mut() {
                    if let Err(e) = v.scrape() {
                        if let Some(target_url) = v.get_url_if_proxy() {
                            log::error!(
                                "Failed to scrape proxy {} : {}! Notifying the root server.",
                                k,
                                e
                            );
                            if let Some(root_url) = self.root_proxy.read().unwrap().as_ref() {
                                if let Some(my_url) = self.web_url.read().unwrap().as_ref() {
                                    if let Err(e) = ExporterFactory::remove_proxy_scrape(
                                        self, root_url, my_url, target_url,
                                    ) {
                                        log::error!("Failed to notify root server about non responsive proxy {}: {}", target_url, e);
                                    }
                                }
                            }
                        }

                        log::debug!("Failed to scrape {} : {}", k, e);
                        to_delete.push(k.to_string());
                    }
                }

                /* Now backpush pending scrapes (traces might be added as we run) */
                if let Ok(pending) = self.pending_scrapes.lock().as_mut() {
                    for (name, scrape) in pending.drain(..) {
                        scrapes.insert(name, scrape);
                    }
                }

                /* Remove failed scrapes */
                for k in to_delete {
                    scrapes.remove(&k);
                }
            }

            sleep(Duration::from_millis(10));
        }
    }

    #[allow(unused)]
    /// Add a new scrape to the scrape list
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

    #[allow(unused)]
    /// Remove a scrape from the scrape list
    pub(crate) fn remove_scrape(
        factory: Arc<ExporterFactory>,
        url: &String,
    ) -> Result<(), Box<dyn Error>> {
        match factory.scrapes.lock().unwrap().remove(url) {
            Some(_) => Ok(()),
            None => Err(ProxyErr::newboxed(format!(
                "No such scrape {} to remove",
                url
            ))),
        }
    }

    #[allow(unused)]
    /// List all scrapes in the scrape list
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

    #[allow(unused)]
    /// This function is called when joining another proxy
    ///
    /// It will first request the target address from the root server
    /// and then it will register itself in the returned address
    /// This function is used to dynamically build the reduction tree
    pub(crate) fn join(
        root_server: &String,
        my_server_address: &String,
        period: u64,
    ) -> Result<(), ProxyErr> {
        let mut pivot_url = root_server.to_string() + "/pivot?from=" + my_server_address;

        if !pivot_url.starts_with("http") {
            pivot_url = format!("http://{}", pivot_url);
        }

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

    fn remove_proxy_scrape(
        &self,
        root_server: &String,
        my_server_address: &String,
        target_address: &String,
    ) -> Result<(), ProxyErr> {
        let mut target_address = target_address.to_string();
        if target_address.starts_with("http://") {
            target_address = target_address.replace("http://", "");
        }
        if target_address.ends_with("/job") {
            target_address = target_address.replace("/job", "");
        }

        let mut pivot_url = root_server.to_string()
            + "/remove?from="
            + my_server_address
            + "&target="
            + &target_address;

        println!("pivot_url: {}", pivot_url);

        if !pivot_url.starts_with("http") {
            pivot_url = format!("http://{}", pivot_url);
        }

        println!(
            "Notifying root server {} about failed proxy {}, we are {}",
            root_server, target_address, my_server_address
        );

        /* We add some delay as the root server may get smashed */
        let resp = retry(Fixed::from_millis(2000).take(5), || {
            ApiResponse::query(&pivot_url)
        })?;

        if resp.success {
            let response: Vec<&str> = resp.operation.split('&').collect();

            let target_url = "http://".to_string()
                + my_server_address
                + "/join?to="
                + response[0]
                + "&period="
                + response[1];

            match ApiResponse::query(&target_url) {
                Ok(_) => {
                    log::info!(
                        "Letting proxy {} join with period {}",
                        response[0],
                        response[1]
                    );
                    return Ok(());
                }
                Err(e) => return Err(ProxyErr::from(e)),
            }
        } else {
            Err(ProxyErr::new(
                format!(
                    "Failed to notify root server about failed proxy {}",
                    target_address
                )
                .as_str(),
            ))
        }
    }

    #[allow(unused)]
    pub(crate) fn set_data(
        factory: Arc<ExporterFactory>,
        root: &String,
        my_server_address: &String,
        period: u64,
    ) -> Result<(), ProxyErr> {
        {
            let mut p = factory.period.write().unwrap();
            *p = period;
        }

        let mut root_url = root.to_string();
        if !root_url.starts_with("http") {
            root_url = format!("http://{}", root_url);
        }

        let mut web_url = my_server_address.to_string();

        match factory.root_proxy.write() {
            Ok(mut guard) => {
                *guard = Some(root_url);
                match factory.web_url.write() {
                    Ok(mut web_guard) => {
                        *web_guard = Some(web_url);
                        Ok(())
                    }
                    Err(e) => {
                        return Err(ProxyErr::new(
                            format!("Failed to set web url: {}", e).as_str(),
                        ));
                    }
                }
            }
            Err(e) => {
                return Err(ProxyErr::new(
                    format!("Failed to set root proxy: {}", e).as_str(),
                ));
            }
        }
    }

    pub(crate) fn new(
        profile_prefix: PathBuf,
        aggregate: bool,
        max_trace_size: usize,
        period: u64,
        branches: u64,
    ) -> Result<Arc<ExporterFactory>, Box<dyn Error>> {
        let main_jobdesc = JobDesc {
            jobid: "main".to_string(),
            command: "Sum of all Jobs".to_string(),
            size: 0,
            nodelist: "".to_string(),
            partition: "".to_string(),
            cluster: "".to_string(),
            run_dir: "".to_string(),
            start_time: 0,
            end_time: 0,
        };

        let nodejob_desc = JobDesc {
            jobid: format!("Node: {}", hostname()),
            command: format!("Sum of all Jobs running on {}", hostname()),
            size: 0,
            nodelist: hostname(),
            partition: "".to_string(),
            cluster: "".to_string(),
            run_dir: "".to_string(),
            start_time: 0,
            end_time: 0,
        };

        let trace_store = Arc::new(TraceView::new(&profile_prefix)?);

        let ftio_client = Arc::new(FtioClient::new("tcp://127.0.0.1:5555"));
        if !ftio_client.ping_server() && which::which("admire_proxy_zmq").is_ok() {
            println!("FTIO server not responding, attempting to start it...");
            Command::new("admire_proxy_zmq")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        }

        let (main_job_trace, node_job_trace) = if aggregate {
            trace_store.clear(&main_jobdesc)?;
            trace_store.clear(&nodejob_desc)?;
            (
                Some(trace_store.get(&main_jobdesc, max_trace_size).unwrap()),
                Some(trace_store.get(&nodejob_desc, max_trace_size).unwrap()),
            )
        } else {
            (None, None)
        };

        let ret = Arc::new(ExporterFactory {
            main: Arc::new(Exporter::new()),
            pernode: Arc::new(Exporter::new()),
            perjob: Mutex::new(HashMap::new()),
            scrapes: Mutex::new(HashMap::new()),
            pending_scrapes: Mutex::new(Vec::new()),
            profile_store: Arc::new(ProfileView::new(&profile_prefix)?),
            trace_store: trace_store.clone(),
            aggregator: aggregate,
            max_trace_size,
            ftio_client: ftio_client.clone(),
            root_proxy: Arc::new(RwLock::new(None)),
            web_url: Arc::new(RwLock::new(None)),
            period: Arc::new(RwLock::new(period)),
            branches,
        });

        let scrape_ref = ret.clone();
        // Start Scraping thread
        std::thread::spawn(move || {
            scrape_ref.run_scrapping();
        });

        ret.insert_ftio_exporter(trace_store.clone(), &main_jobdesc.jobid)?;
        ret.insert_ftio_exporter(trace_store.clone(), &nodejob_desc.jobid)?;

        /* This creates a job entry for the cumulative job */
        let main_job = PerJobRefcount {
            desc: main_jobdesc,
            exporter: ret.main.clone(),
            counter: 1,
            islocal: false,
        };
        ret.perjob
            .lock()
            .unwrap()
            .insert(main_job.desc.jobid.to_string(), main_job);

        /* This creates a job entry for the pernode job */
        let node_job = PerJobRefcount {
            desc: nodejob_desc,
            exporter: ret.pernode.clone(),
            counter: 1,
            islocal: false,
        };
        ret.perjob
            .lock()
            .unwrap()
            .insert(node_job.desc.jobid.to_string(), node_job);

        /* Now insert the default system scrape */
        let systemurl = "/system".to_string();
        if let Ok(sys_metrics) =
            ProxyScraper::new(&systemurl, proxy_common::get_proxy_period(), ret.clone())
        {
            ret.scrapes.lock().unwrap().insert(systemurl, sys_metrics);
        }

        /* Now insert tracing events */
        ret.insert_tracing(ret.main.clone(), main_job_trace)?;
        ret.insert_tracing(ret.pernode.clone(), node_job_trace)?;

        Ok(ret)
    }

    fn insert_tracing(
        &self,
        exporter: Arc<Exporter>,
        trace: Option<Arc<Trace>>,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(trace) = trace {
            if let Ok(main_trace_scraper) = ProxyScraper::newtrace(exporter, trace) {
                self.pending_scrapes
                    .lock()
                    .unwrap()
                    .push((main_trace_scraper.url().to_string(), main_trace_scraper));
            }
        }

        Ok(())
    }

    fn insert_ftio_exporter(
        &self,
        exporter: Arc<TraceView>,
        jobid: &String,
    ) -> Result<(), Box<dyn Error>> {
        if let Ok(ftio_scrapper) = ProxyScraper::newftio(exporter, jobid, self.ftio_client.clone()) {
            self.pending_scrapes
                .lock()
                .unwrap()
                .push((ftio_scrapper.url().to_string(), ftio_scrapper));
        }

        Ok(())
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
                    e.islocal = true;
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
                let trace = if self.aggregator {
                    self.trace_store.get(desc, self.max_trace_size).ok()
                } else {
                    None
                };

                let new: PerJobRefcount = PerJobRefcount {
                    desc: desc.clone(),
                    exporter: Arc::new(Exporter::new()),
                    counter: 1,
                    islocal: tobesaved,
                };

                /* Add the trace scrapping */
                self.insert_tracing(new.exporter.clone(), trace).unwrap();

                self.insert_ftio_exporter(self.trace_store.clone(), &desc.jobid)
                    .unwrap_or(());

                let ret = new.exporter.clone();
                ht.insert(desc.jobid.to_string(), new);

                ret
            }
        };

        v
    }

    #[allow(unused)]
    pub(crate) fn list_jobs(&self) -> Vec<JobDesc> {
        self.perjob
            .lock()
            .unwrap()
            .values()
            .map(|k| k.desc.clone())
            .collect()
    }

    #[allow(unused)]
    pub(crate) fn profiles(&self, full: bool) -> Vec<JobProfile> {
        let mut ret: Vec<JobProfile> = Vec::new();

        if let Ok(ht) = self.perjob.lock() {
            for v in ht.values() {
                if let Ok(p) = v.profile(full) {
                    ret.push(p);
                }
            }
        }

        ret
    }

    #[allow(unused)]
    pub(crate) fn profile_of(&self, jobid: &str, full: bool) -> Result<JobProfile, ProxyErr> {
        if let Some(elem) = self.perjob.lock().unwrap().get(jobid) {
            return elem.profile(full);
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
                    if self.aggregator {
                        let snap = perjob.exporter.profile(desc, false)?;
                        self.profile_store.saveprofile(snap, desc)?;
                        self.trace_store.done(desc)?;
                    }
                    /* Delete */
                    ht.remove(&desc.jobid);
                }
            }
        } else {
            return Err(ProxyErr::newboxed("No such job to remove"));
        }

        Ok(())
    }

    #[allow(unused)]
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

    #[allow(unused)]
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

    #[allow(unused)]
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

    #[allow(unused)]
    pub(crate) fn check_alarms(&self) -> HashMap<String, Vec<ValueAlarmTrigger>> {
        let mut ret: HashMap<String, Vec<ValueAlarmTrigger>> = HashMap::new();

        let perjobht = self.perjob.lock().unwrap();

        for (k, v) in perjobht.iter() {
            let alarms: Vec<ValueAlarmTrigger> = v.exporter.check_alarms();
            ret.insert(k.to_string(), alarms);
        }

        ret
    }

    #[allow(unused)]
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

    pub(crate) fn get_local_job_exporters(
        &self,
    ) -> Result<Vec<Arc<Exporter>>, Box<dyn Error + '_>> {
        let e = self.perjob.try_lock()?;

        Ok(e.iter()
            .filter(|(_, v)| v.islocal)
            .map(|(_, v)| v.exporter.clone())
            .collect())
    }

    #[allow(unused)]
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
