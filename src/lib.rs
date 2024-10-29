use elf::abi::PT_LOAD;
use elf::endian::AnyEndian;
use elf::segment::ProgramHeader;
use lazy_static::lazy_static;
use proc_maps::{get_process_maps, maps_contain_addr, MapRange};
use std::env;
use std::ffi::CStr;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use std::{error::Error, io::Write};
mod proxy_common;
mod squeue;
use elf::ElfBytes;
use proxy_common::ProxyErr;
use proxy_common::{get_proxy_path, init_log};

mod proxywireprotocol;
use libc::{c_ulonglong, signal, user, SIGPIPE, SIG_IGN};
use proxywireprotocol::{CounterType, CounterValue, JobDesc, ProxyCommand, ValueDesc};

use std::collections::{HashMap, HashSet};

use std::thread;

use std::sync::Once;

pub struct MetricProxyValue {
    value: Mutex<CounterValue>,
}

impl MetricProxyValue {
    fn newcounter(name: String) -> MetricProxyValue {
        MetricProxyValue {
            value: Mutex::new(CounterValue {
                name,
                value: CounterType::newcounter(),
            }),
        }
    }

    fn updated(&self) -> bool {
        let val = self.value.lock().unwrap();
        match val.value {
            CounterType::Counter { ts: _, value } => value > 0.0,
            CounterType::Gauge {
                min: _,
                max: _,
                hits,
                total: _,
            } => hits > 0.0,
        }
    }

    fn newgauge(name: String) -> MetricProxyValue {
        MetricProxyValue {
            value: Mutex::new(CounterValue {
                name,
                value: CounterType::newgauge(),
            }),
        }
    }

    fn inc(&self, increment: f64) -> Result<(), ProxyErr> {
        let mut tval = self.value.lock().unwrap();

        match &mut tval.value {
            CounterType::Counter { ts: _, value } => {
                *value += increment;
            }
            _ => {
                return Err(ProxyErr::new("Inc is only meaningfull for counters"));
            }
        }

        Ok(())
    }

    fn set(&self, value: f64) -> Result<(), ProxyErr> {
        let mut tval = self.value.lock().unwrap();

        let new = CounterType::Gauge {
            min: value,
            max: value,
            hits: 1.0,
            total: value,
        };

        tval.value.set(&new)?;

        Ok(())
    }
}

static mut PROXY_INSTANCE: Option<Arc<MetricProxyClient>> = None;

pub struct MetricProxyClient {
    period: Duration,
    running: Arc<Mutex<bool>>,
    stream: Mutex<Option<UnixStream>>,
    counters: RwLock<HashMap<String, Arc<MetricProxyValue>>>,
    functions: RwLock<HashMap<String, Arc<MetricProxyValue>>>,
    maps: Vec<MapRange>,
}

impl Drop for MetricProxyClient {
    fn drop(&mut self) {
        self.dump_values().ok();
    }
}

static START: Once = Once::new();

lazy_static! {
    static ref JOBDESC: JobDesc = JobDesc::new();
}

impl MetricProxyClient {
    fn new() -> Arc<MetricProxyClient> {
        unsafe {
            if let Some(client) = PROXY_INSTANCE.clone() {
                return client;
            }
        }

        START.call_once(|| {
            init_log();
        });

        unsafe {
            signal(SIGPIPE, SIG_IGN);
        }

        let mut can_run: bool = true;

        let sock_path = env::var("PROXY_PATH").unwrap_or(get_proxy_path());
        let path = Path::new(&sock_path);

        let tsock = if !path.exists() {
            None
        } else {
            match UnixStream::connect(path) {
                Ok(v) => Some(v),
                Err(e) => {
                    log::error!("Failed to connect : {}", e);
                    None
                }
            }
        };

        if tsock.is_none() {
            can_run = false;
            log::warn!("Not Connected to Metric Proxy");
        }

        let period: Duration = Duration::from_millis(proxy_common::get_proxy_period());

        let client = MetricProxyClient {
            period,
            running: Arc::new(Mutex::new(can_run)),
            stream: Mutex::new(tsock),
            counters: RwLock::new(HashMap::new()),
            functions: RwLock::new(HashMap::new()),
            maps: get_process_maps(std::process::id() as i32).unwrap(),
        };

        let pclient = Arc::new(client);
        let rclient = pclient.clone();

        /* No need to start the thread if we do not run */
        if pclient.running() {
            /* Send initial jobdesc  */
            pclient.send_jobdesc().ok();
            thread::spawn(move || {
                while rclient.running() {
                    if rclient.dump_values().is_err() {
                        break;
                    }
                    thread::sleep(rclient.period);
                }
                log::info!("Polling thread leaving");
            });
        }

        unsafe {
            PROXY_INSTANCE = Some(pclient.clone());
        }

        pclient
    }

    fn text_offset(dso: &str) -> Option<usize> {
        let path = std::path::PathBuf::from(dso);

        if !path.is_file() {
            return None;
        }

        let file_data = std::fs::read(path).expect("Could not read file.");
        let slice = file_data.as_slice();
        if let Ok(file) = ElfBytes::<AnyEndian>::minimal_parse(slice) {
            if let Ok(Some(txt)) = file.section_header_by_name(".text") {
                let p_vaddr = if let Some(first_load_phdr) =
                    file.segments().unwrap().iter().find(|phdr| {
                        phdr.p_type == PT_LOAD
                            && ((phdr.p_offset <= txt.sh_offset)
                                && ((txt.sh_offset + txt.sh_size)
                                    <= (phdr.p_offset + phdr.p_filesz)))
                    }) {
                    first_load_phdr.p_offset
                } else {
                    0
                };

                log::debug!(
                    "{} .txt is at {:#x} loaded {:#x} linked at {:#x}",
                    dso,
                    txt.sh_offset,
                    txt.sh_addr,
                    txt.sh_offset
                );
                return Some((txt.sh_offset) as usize);
            }
        }

        None
    }

    fn dso_local_offset(&self, addr: usize) -> (usize, String) {
        for r in self.maps.iter() {
            if maps_contain_addr(addr, &[r.clone()]) {
                let p = if let Some(path) = r.filename().unwrap().as_os_str().to_str() {
                    path.to_string()
                } else {
                    "Unknown".to_string()
                };

                log::debug!("{} load is at {:#x}", p, r.start());

                let faddr = if let Some(file_off) = MetricProxyClient::text_offset(&p) {
                    log::debug!(
                        "Symbol is at offset {:#x} in loaded section",
                        addr - r.start()
                    );
                    (addr - r.start()) + file_off
                } else {
                    addr
                };

                return (faddr, p);
            }
        }

        (addr, "Unknown".to_string())
    }

    fn dump_values(&self) -> Result<(), Box<dyn Error>> {
        let values_to_send: Vec<ProxyCommand>;
        {
            values_to_send = self
                .counters
                .read()
                .unwrap()
                .iter()
                .filter(|(_, v)| v.updated())
                .map(|(_, v)| {
                    let mut value = v.value.lock().unwrap();
                    let ts = proxy_common::unix_ts_us();
                    let ret = ProxyCommand::Value(value.set_ts(ts).clone());
                    /* Make sure to clear the original counter */
                    value.reset();
                    ret
                })
                .collect();
        }

        for command in values_to_send {
            self.send(&command)?;
        }
        Ok(())
    }

    fn running(&self) -> bool {
        return *self.running.lock().unwrap();
    }

    fn send(&self, cmd: &ProxyCommand) -> Result<(), Box<dyn Error>> {
        let mut stream_lock = self.stream.lock().unwrap();

        if let Some(mut stream) = stream_lock.as_mut() {
            serde_json::to_writer(&mut stream, cmd)?;
            let null_byte: [u8; 1] = [0_u8; 1];
            stream.write_all(&null_byte)?;

            log::debug!("Sending {:?}", cmd);
        } else {
            *self.running.lock().unwrap() = false;
            return Err(ProxyErr::newboxed("Not connected to UNIX socket"));
        }

        Ok(())
    }

    fn send_jobdesc(&self) -> Result<(), Box<dyn Error>> {
        let desc = ProxyCommand::JobDesc(JOBDESC.clone());
        self.send(&desc)
    }

    fn push_entry(
        &self,
        name: String,
        doc: String,
        ctype: CounterType,
    ) -> Result<Arc<MetricProxyValue>, Box<dyn Error>> {
        let counter: Arc<MetricProxyValue>;

        let command = ProxyCommand::Desc(ValueDesc {
            name: name.to_string(),
            doc,
            ctype: ctype.clone(),
        });

        /* First try to add the counters */
        {
            let mut ht = self.counters.write().unwrap();

            let foundcounter = ht.get_mut(&name);

            if foundcounter.is_none() {
                counter = match ctype {
                    CounterType::Counter { .. } => {
                        Arc::new(MetricProxyValue::newcounter(name.to_string()))
                    }
                    CounterType::Gauge { .. } => {
                        Arc::new(MetricProxyValue::newgauge(name.to_string()))
                    }
                };
                ht.insert(name.to_string(), counter.clone());
            } else {
                counter = foundcounter.cloned().unwrap();
            }
        }

        self.send(&command)?;

        Ok(counter)
    }

    fn new_counter(
        &self,
        name: String,
        doc: String,
    ) -> Result<Arc<MetricProxyValue>, Box<dyn Error>> {
        self.push_entry(name, doc, CounterType::newcounter())
    }

    fn new_gauge(
        &mut self,
        name: String,
        doc: String,
    ) -> Result<Arc<MetricProxyValue>, Box<dyn Error>> {
        self.push_entry(name, doc, CounterType::newgauge())
    }

    fn addr2line(addr: usize, dso: &str) -> String {
        let mut command = std::process::Command::new("addr2line");
        command.arg("-fe").arg(dso).arg(format!("0x{:x}", addr));

        let clean_dso = dso.replace(".so", "").replace("/", "_").replace(".", "_");

        if let Ok(output) = command.output() {
            let output_str = String::from_utf8_lossy(&output.stdout);
            let lines: Vec<&str> = output_str.split('\n').collect();

            if lines.len() > 1 {
                if lines[0].contains("??") {
                    return format!("{:#x}_{}", addr, clean_dso);
                } else {
                    return format!("{}_{}", lines[0], clean_dso);
                }
            }
        }

        format!("{:#x}{}", addr, clean_dso)
    }

    fn new_func(
        &self,
        this_fn: usize,
        callsite: usize,
    ) -> Result<Arc<MetricProxyValue>, Box<dyn Error>> {
        let func: String = format!("{}@{}", this_fn, callsite);

        if let Ok(funcs) = self.functions.read() {
            if let Some(prev) = funcs.get(&func) {
                return Ok(prev.clone());
            }
        }

        let (addr, dso) = self.dso_local_offset(this_fn);

        let locus = MetricProxyClient::addr2line(addr, &dso);

        log::trace!("CALLSITE {}", locus);

        if let Ok(c) = self.new_counter(
            format!("func__{}", locus.clone()),
            format!("Number of calls to {}", locus),
        ) {
            self.functions
                .write()
                .as_mut()
                .unwrap()
                .insert(func.clone(), c.clone());
            return Ok(c);
        }

        Err(ProxyErr::newboxed("Failed to retrieve proxyclient"))
    }
}

/// This intanciates the metric client
///
/// # Return
///
/// An opaque object representing the metric client
#[no_mangle]
pub extern "C" fn metric_proxy_init() -> *mut MetricProxyClient {
    let client = MetricProxyClient::new();
    let client = Arc::into_raw(client) as *mut MetricProxyClient;

    let pclient: &mut MetricProxyClient = unsafe { &mut *(client) };

    if let Ok(start) = pclient.new_counter(
        "has_started".to_string(),
        "Number of calls to metric_proxy_init".to_string(),
    ) {
        let _ = start.inc(1.0);
    }

    client
}

/// Release the metric proxy
///
/// # Arguments
///
/// - pclient: a pointer to the metric client as returned by `metric_proxy_init`
/// # Safety
///
/// Only pointer returned by `metric_proxy_init` should be passed.
#[no_mangle]
pub unsafe extern "C" fn metric_proxy_release(pclient: *mut MetricProxyClient) -> std::ffi::c_int {
    let zero: std::ffi::c_int = 0;
    let one: std::ffi::c_int = 1;

    /* Get the client back and drop it */
    let client: &mut MetricProxyClient = unsafe { &mut *(pclient) };

    if let Ok(done) = client.new_counter(
        "has_finished".to_string(),
        "Number of calls to metric_proxy_release".to_string(),
    ) {
        let _ = done.inc(1.0);
    }

    *client.running.lock().unwrap() = false;

    if client.dump_values().is_err() {
        return one;
    }

    zero
}

fn unwrap_c_string(pcstr: *const std::os::raw::c_char) -> Result<String, Box<dyn Error>> {
    // Convert the `char*` to a Rust CStr
    let cstr = unsafe { CStr::from_ptr(pcstr) };

    // Convert the CStr to a Rust &str (Unicode)
    match cstr.to_str() {
        Ok(e) => Ok(e.to_string()),
        Err(e) => Err(Box::new(e)),
    }
}

/* Counters */

/// Create a new Cointer from the metric client
///
/// # Arguments
///
/// - pclient: a pointer to the metric client as returned by `metric_proxy_init`
/// - name : name of the counter
/// - doc: documentation of the counter
///
/// # Returns
///
/// - Opaque pointer to a Counter instance
///
/// # Safety
///
/// Only correct pointers are returned by previous functions should be returned.
/// Doing otherwise may crash.
#[no_mangle]
pub unsafe extern "C" fn metric_proxy_counter_new(
    pclient: *mut MetricProxyClient,
    name: *const std::os::raw::c_char,
    doc: *const std::os::raw::c_char,
) -> *mut MetricProxyValue {
    let rname = unwrap_c_string(name);
    let rdoc = unwrap_c_string(doc);

    if rname.is_err() || rdoc.is_err() || pclient.is_null() {
        return std::ptr::null_mut();
    }

    let client: &mut MetricProxyClient = unsafe { &mut *(pclient) };

    if !*client.running.lock().unwrap() {
        return std::ptr::null_mut();
    }

    let rname = rname.unwrap();
    let rdoc = rdoc.unwrap();

    if let Ok(c) = client.new_counter(rname, rdoc) {
        return Arc::into_raw(c) as *mut MetricProxyValue;
    }

    std::ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn rust_ctor() {
    log::debug!("Calling constructor for proxy_client library");
    let _ = MetricProxyClient::new();
}

#[no_mangle]
pub extern "C" fn rust_dtor() {
    log::debug!("Calling destructor for proxy_client library");
    unsafe {
        if let Some(client) = PROXY_INSTANCE.clone() {
            let _ = client.dump_values();
        }
    }
}

#[link_section = ".init_array"]
pub static INITIALIZE: extern "C" fn() = rust_ctor;

#[link_section = ".fini_array"]
pub static FINALIZE: extern "C" fn() = rust_dtor;

/// This creates a counter for the given function if it does not exists
///
/// # Safety
///
/// Only correct pointers are returned by previous functions should be returned.
/// Doing otherwise may crash.
#[no_mangle]
pub unsafe extern "C" fn metric_proxy_get_func(
    pclient: *mut MetricProxyClient,
    func: libc::size_t,
    callsite: libc::size_t,
) -> *mut MetricProxyValue {
    let client: &mut MetricProxyClient = unsafe { &mut *(pclient) };

    if !*client.running.lock().unwrap() {
        return std::ptr::null_mut();
    }

    if let Ok(c) = client.new_func(func, callsite) {
        return Arc::into_raw(c) as *mut MetricProxyValue;
    }

    std::ptr::null_mut()
}

/// Callback function for entering a function.new_func
#[no_mangle]
pub extern "C" fn __cyg_profile_func_enter(this_fn: *const (), call_site: *const ()) {
    log::trace!("==> FUNC ENTER {:p} && {:p}", this_fn, call_site);
    unsafe {
        let client = if let Some(client) = PROXY_INSTANCE.clone() {
            client
        } else {
            MetricProxyClient::new()
        };

        let this_fn: usize = this_fn as usize;
        let call_site: usize = call_site as usize;

        // Additional logic using `fn_addr` can be added here

        if let Ok(cnt) = client.new_func(this_fn, call_site) {
            let _ = cnt.inc(1.0);
        }
    }
}

/// This Increments the value of a Counter in the proxy
/// This refers to a value previously created with `metric_proxy_gauge_new`
///
/// # Arguments
///
/// - pcounter: the gauge to update (as returned by `metric_proxy_gauge_new`)
/// - value: the value to add to current value
///
/// # Safety
/// If a wrong pointer is passed behavior is undefined (and may crash)
#[no_mangle]
pub unsafe extern "C" fn metric_proxy_counter_inc(
    pcounter: *mut MetricProxyValue,
    value: std::ffi::c_double,
) -> std::ffi::c_int {
    let zero: std::ffi::c_int = 0;
    let one: std::ffi::c_int = 1;

    if pcounter.is_null() {
        return one;
    }

    let counter: &mut MetricProxyValue = unsafe { &mut *(pcounter) };

    if counter.inc(value).is_err() {
        return one;
    }

    zero
}

/* Gauges  */

/// Create a new Gauge from the metric client
///
/// # Arguments
///
/// - pclient: a pointer to the metric client as returned by `metric_proxy_init`
/// - name : name of the gauge
/// - doc: documentation of the gauge
///
/// # Returns
///
/// - Opaque pointer to a Gauge instance
///
/// # Safety
///
/// Only correct pointers are returned by previous functions should be returned.
/// Doing otherwise may crash.
#[no_mangle]
pub unsafe extern "C" fn metric_proxy_gauge_new(
    pclient: *mut MetricProxyClient,
    name: *const std::os::raw::c_char,
    doc: *const std::os::raw::c_char,
) -> *mut MetricProxyValue {
    let rname = unwrap_c_string(name);
    let rdoc = unwrap_c_string(doc);

    if rname.is_err() || rdoc.is_err() || pclient.is_null() {
        return std::ptr::null_mut();
    }

    let client: &mut MetricProxyClient = unsafe { &mut *(pclient) };

    if !*client.running.lock().unwrap() {
        return std::ptr::null_mut();
    }

    let rname = rname.unwrap();
    let rdoc = rdoc.unwrap();

    if let Ok(c) = client.new_gauge(rname, rdoc) {
        return Arc::into_raw(c) as *mut MetricProxyValue;
    }

    std::ptr::null_mut()
}

/// This set the value of a Gauge in the proxy
/// This refers to a value previously created with `metric_proxy_gauge_new`
///
/// # Arguments
///
/// - pcounter: the gauge to update (as returned by `metric_proxy_gauge_new`)
/// - value: the value to set
///
/// # Safety
/// If a wrong pointer is passed behavior is undefined (and may crash)
#[no_mangle]
pub unsafe extern "C" fn metric_proxy_gauge_set(
    pcounter: *mut MetricProxyValue,
    value: std::ffi::c_double,
) -> std::ffi::c_int {
    let zero: std::ffi::c_int = 0;
    let one: std::ffi::c_int = 1;

    if pcounter.is_null() {
        return one;
    }

    let gauge: &mut MetricProxyValue = unsafe { &mut *(pcounter) };

    if gauge.set(value).is_err() {
        return one;
    }

    zero
}
