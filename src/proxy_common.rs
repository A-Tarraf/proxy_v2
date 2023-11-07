use std::ffi::OsStr;
use std::fs;
use std::process::exit;
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
pub(crate) fn create_dir_or_fail(path: &PathBuf) {
    if let Err(e) = std::fs::create_dir(path) {
        log::error!(
            "Failed to create directory at {} : {}",
            path.to_str().unwrap_or(""),
            e
        );
        exit(1);
    }
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
