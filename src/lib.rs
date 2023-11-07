use std::env;
use std::ffi::CStr;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use std::{error::Error, io::Write};

mod proxy_common;
use proxy_common::ProxyErr;
use proxy_common::{get_proxy_path, init_log};

mod proxywireprotocol;
use libc::{signal, SIGPIPE, SIG_IGN};
use proxywireprotocol::{CounterType, CounterValue, JobDesc, ProxyCommand, ValueDesc};

use std::collections::HashMap;

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
            CounterType::Counter { value } => value > 0.0,
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

        match tval.value {
            CounterType::Counter { value } => {
                tval.value = CounterType::Counter {
                    value: value + increment,
                }
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

pub struct MetricProxyClient {
    period: Duration,
    running: Arc<Mutex<bool>>,
    stream: Mutex<Option<UnixStream>>,
    counters: RwLock<HashMap<String, Arc<MetricProxyValue>>>,
}

impl Drop for MetricProxyClient {
    fn drop(&mut self) {
        self.dump_values().ok();
    }
}

static START: Once = Once::new();

impl MetricProxyClient {
    fn new() -> Arc<MetricProxyClient> {
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

        let period: Duration = env::var("PROXY_PERIOD")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_millis(1000));

        let client = MetricProxyClient {
            period,
            running: Arc::new(Mutex::new(can_run)),
            stream: Mutex::new(tsock),
            counters: RwLock::new(HashMap::new()),
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

        pclient
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
                    let ret = ProxyCommand::Value(value.clone());
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
            let null_byte: [u8; 1] = [0; 1];
            stream.write_all(&null_byte)?;

            log::debug!("Sending {:?}", cmd);
        } else {
            *self.running.lock().unwrap() = false;
            return Err(ProxyErr::newboxed("Not connected to UNIX socket"));
        }

        Ok(())
    }

    fn send_jobdesc(&self) -> Result<(), Box<dyn Error>> {
        let desc = ProxyCommand::JobDesc(JobDesc::new());
        return self.send(&desc);
    }

    fn push_entry(
        &mut self,
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
        &mut self,
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
}

#[no_mangle]
pub extern "C" fn metric_proxy_init() -> *mut MetricProxyClient {
    let client = MetricProxyClient::new();
    Arc::into_raw(client) as *mut MetricProxyClient
}

#[no_mangle]
pub unsafe extern "C" fn metric_proxy_release(pclient: *mut MetricProxyClient) -> std::ffi::c_int {
    let zero: std::ffi::c_int = 0;
    let one: std::ffi::c_int = 1;

    /* Get the client back and drop it */
    let client: Arc<MetricProxyClient> = unsafe { Arc::from_raw(pclient) };

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
