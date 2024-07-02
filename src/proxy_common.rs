use std::ffi::OsStr;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{error::Error, path::PathBuf};

/*******************
 * IMPLEMENT ERROR *
 *******************/

#[derive(Debug)]
pub(crate) struct ProxyErr {
    message: String,
}

impl Error for ProxyErr {}

impl ProxyErr {
    // Create a constructor method for your custom error
    #[allow(unused)]
    pub(crate) fn new<T: ToString>(message: T) -> ProxyErr {
        ProxyErr {
            message: message.to_string(),
        }
    }

    #[allow(unused)]
    pub(crate) fn newboxed<T: ToString>(message: T) -> Box<ProxyErr> {
        Box::new(ProxyErr {
            message: message.to_string(),
        })
    }
}

impl std::fmt::Display for ProxyErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl From<Box<dyn std::error::Error>> for ProxyErr {
    fn from(err: Box<dyn std::error::Error>) -> Self {
        ProxyErr::new(err)
    }
}

impl From<retry::Error<Box<dyn std::error::Error>>> for ProxyErr {
    fn from(err: retry::Error<Box<dyn std::error::Error>>) -> Self {
        ProxyErr::new(err.to_string().as_str()) // Adjust this as per your ProxyErr constructor.
    }
}

#[allow(unused)]
pub fn init_log() {
    let env = env_logger::Env::new().default_filter_or("info");
    env_logger::init_from_env(env);
}

#[allow(unused)]
pub fn unix_ts() -> u64 {
    let current_time = SystemTime::now();
    current_time
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}

#[allow(unused)]
pub fn unix_ts_us() -> u128 {
    let current_time = SystemTime::now();
    current_time
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_micros()
}

#[allow(unused)]
pub(crate) fn list_files_with_ext_in(
    path: &PathBuf,
    ext: &str,
) -> Result<Vec<String>, Box<dyn Error>> {
    if !path.is_dir() {
        return Err(ProxyErr::newboxed("Aggregator path is not a directory"));
    }

    let mut ret: Vec<String> = Vec::new();

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let fname = PathBuf::from(entry.file_name());

        let mut full_path = path.clone();
        full_path.push(fname);

        if full_path.extension().unwrap_or(OsStr::new("")) == ext {
            ret.push(full_path.to_string_lossy().to_string());
        }
    }

    Ok(ret)
}

#[allow(unused)]
pub(crate) fn create_dir_or_fail(path: &PathBuf) -> Result<(), ProxyErr> {
    if let Err(e) = std::fs::create_dir(path) {
        return Err(ProxyErr::new(format!(
            "Failed to create directory at {} : {}",
            path.to_str().unwrap_or(""),
            e
        )));
    }

    Ok(())
}

#[allow(unused)]
pub(crate) fn check_prefix_dir(prefix: &PathBuf, dirname: &str) -> Result<PathBuf, ProxyErr> {
    // Main directory
    if !prefix.exists() {
        create_dir_or_fail(prefix)?;
    } else if !prefix.is_dir() {
        return Err(ProxyErr::new(format!(
            "{} is not a directory cannot use it as {} prefix",
            dirname,
            prefix.to_str().unwrap_or("")
        )));
    }

    // Profile subdirectory
    let mut target_dir = prefix.clone();
    target_dir.push(dirname);

    if !target_dir.exists() {
        create_dir_or_fail(&target_dir)?;
    }

    Ok(target_dir)
}

#[allow(unused)]
pub(crate) fn hostname() -> String {
    let host: std::ffi::OsString = gethostname::gethostname();
    let strh = host.to_str().unwrap_or("unknown");
    strh.to_string()
}

#[allow(unused)]
pub(crate) fn is_url_live(url: &str, html: bool) -> Result<(), Box<dyn Error>> {
    let client = reqwest::blocking::Client::new();
    let response = client.get(url).send()?;

    if response.status().is_success() {
        let txt = response.text().unwrap();
        let has_html = txt.contains("<html>");
        if html == has_html {
            Ok(())
        } else {
            Err(ProxyErr::newboxed(format!(
                "Response {} HTML {:?} expected {:?} over {}",
                url, has_html, html, txt
            )))
        }
    } else {
        Err(ProxyErr::newboxed(
            format!(
                "Failed to connect to {} got response {}",
                url,
                response.status()
            )
            .as_str(),
        ))
    }
}

#[allow(unused)]
pub fn concat_slices(slices: [&'static [u8]; 3]) -> Vec<u8> {
    let mut concatenated_data = Vec::new();

    for slice in slices.iter() {
        concatenated_data.extend_from_slice(slice);
    }

    concatenated_data
}

#[allow(unused)]
pub fn get_proxy_path() -> String {
    let uid = users::get_current_uid();
    format!("/tmp/metric-proxy-{}.socket", uid)
}

#[allow(unused)]
pub fn parse_bool(sbool: &str) -> bool {
    matches!(sbool, "1" | "true")
}

#[allow(unused)]
pub fn derivate_time_serie(data: &Vec<(u64, f64)>) -> Vec<(u64, f64)> {
    let mut ret: Vec<(u64, f64)> = vec![(data[0].0, 0.0)];

    for i in (1..data.len()) {
        let deltax = (data[i].0 as f64) - (data[i - 1].0 as f64);
        ret.push((data[i].0, (data[i].1 - data[i - 1].1) / deltax))
    }

    ret
}

#[allow(unused)]
pub fn offset_time_serie(data: &mut Vec<(u64, f64)>, offset: u64) {
    for v in data {
        v.0 -= offset;
    }
}

pub fn gen_range(start: f64, end: f64, step: f64) -> Result<Vec<f64>, ProxyErr> {
    let mut ret: Vec<f64> = Vec::new();
    let mut v = start;

    if end < start {
        return Err(ProxyErr::new("End cannot be smaller than start"));
    }

    if step <= 0.0 {
        return Err(ProxyErr::new("End cannot be smaller than start"));
    }

    while v < end {
        ret.push(v);
        v += step;
    }

    Ok(ret)
}

pub fn getppid() -> Result<u32, Box<dyn Error>> {
    let id = std::process::id();

    let path_to_status = format!("/proc/{}/task/{}/status", id, id);

    let st = String::from_utf8(std::fs::read(path_to_status)?)?;

    for v in st.split('\n') {
        if v.starts_with("PPid:") {
            let sid = v.replace("PPid:", "").trim().to_string();
            match sid.parse::<u32>() {
                Ok(v) => {
                    return Ok(v);
                }
                Err(e) => {
                    return Err(ProxyErr::newboxed(e));
                }
            }
        }
    }

    Err(ProxyErr::newboxed(
        "Could not find PPID entry in /proc/self/status",
    ))
}
