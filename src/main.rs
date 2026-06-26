use std::env;
use std::error::Error;
use std::path::PathBuf;
use std::process::exit;
use std::sync::{Arc, Mutex};
use std::thread::{self, sleep};
use std::time::Duration;
use std::sync::atomic::{AtomicBool, Ordering};
mod proxy_common;
use proxy_common::{get_proxy_path, init_log};

mod exporter;
use exporter::ExporterFactory;

mod proxy;
use proxy::UnixProxy;

mod squeue;

mod webserver;
use webserver::Web;

mod ftio;
mod extrap;
mod icc;
mod profiles;
mod proxywireprotocol;
mod scrapper;
mod systemmetrics;
mod trace;

extern crate clap;

use clap::Parser;

use crate::exporter::{ExperimentInstrumentation, Instrumentation, NoInstrumentation};
#[cfg(feature = "admire")]
use crate::icc::IccInterface;

extern crate ctrlc;

/// ADMIRE project Instrumentation Proxy
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    // Port number of the HTTP server
    #[arg(short, long, default_value_t = 1337)]
    port: u32,

    // Path of the UNIX proxy for the gateway
    #[arg(short, long)]
    unix: Option<String>,

    /// If set the proxy will attempt to connect to the ADMIRE intelligent controller (needs admire feature)
    #[arg(short, long, default_value_t = false)]
    connect_to_intelligent_controller: bool,

    /// Should profile aggregation be deactivated
    #[arg(short, long, default_value_t = false)]
    inhibit_profile_agreggation: bool,

    /// Subservers to be scrapped (optionnal comma separated list) use ADDR\@[PERIOD in ms] to set the scraping period
    #[arg(short, long, value_delimiter = ',')]
    sub_proxies: Option<Vec<String>>,

    /// Address of the proxy to pivot on to build a proxy tree use ADDR\@[PERIOD in ms] to set the scraping period
    #[arg(short, long)]
    root_proxy: Option<String>,

    /// Maximum trace size to maintain in the file-system in MB (default 32MB)
    #[arg(short, long)]
    max_trace_size: Option<f64>,

    /// Root directory for the proxy (optionnal default ~/.proxyprofiles/)
    #[arg(short, long)]
    target_prefix: Option<PathBuf>,

    /// Sampling period in MS
    #[arg(short = 'S', long, default_value_t = 1000)]
    sampling_period: u64,

    /// Number of branches for the hierarchical aggregation, 0 = binomial tree, > 0 = k-ary tree
    #[arg(short, long, default_value_t = 2)]
    branches: u64,

    /// Duration to run instrumentation in seconds (default 0 = disabled)
    #[arg(long, default_value_t = 0)]
    instrumentation: u64,

    /// Auto-discover the root proxy URL from <target_prefix>/root.url (written by the root proxy)
    /// Also honoured via the PROXY_ROOT_URL environment variable.
    #[arg(long, default_value_t = false)]
    auto_root: bool,

    /// Directory to search for root.url when using --auto-root (defaults to <target-prefix>).
    /// Use this to point all nodes at the root proxy's profile directory on a shared filesystem.
    #[arg(long)]
    root_url_dir: Option<PathBuf>,
}

fn parse_period(arg: &String, default_period: u64) -> (String, u64) {
    let mut spl = arg.split('@');

    let url = spl.next();
    let stime = spl.next();

    if url.is_none() || stime.is_none() {
        return (arg.to_string(), 100);
    }

    match str::parse::<u64>(stime.unwrap()) {
        Ok(v) => (url.unwrap().to_string(), v),
        Err(e) => {
            log::error!("Failed to parse scrape time in {} : {}", arg, e);
            (arg.to_string(), default_period)
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    init_log();

    let args = Args::parse();

    /* Make sure it is globally visible */
    env::set_var("PROXY_PERIOD", format!("{}", args.sampling_period));

    let profile_prefix = if let Some(prefix) = args.target_prefix {
        prefix
    } else {
        let mut d = dirs::home_dir().unwrap();
        d.push(".proxyprofiles");
        d
    };

    let max_trace_size = if let Some(max_size) = args.max_trace_size {
        max_size * 1024.0 * 1024.0
    } else {
        // Default is 32 Mb
        1024.0 * 1024.0 * 32.0
    };

    log::info!(
        "Max trace size is {} MB",
        max_trace_size / (1024.0 * 1024.0)
    );

    let instrumentation: Arc<dyn Instrumentation> =
    if args.instrumentation > 0 {
        Arc::new(ExperimentInstrumentation::new(args.instrumentation))
    } else {
        Arc::new(NoInstrumentation)
    };

    // Keep the root.url file path before profile_prefix is consumed by ExporterFactory::new
    let root_url_file = profile_prefix.join("root.url");

    // The central storage is the exporter
    let factory = ExporterFactory::new(
        profile_prefix,
        !args.inhibit_profile_agreggation,
        max_trace_size as usize,
        args.sampling_period,
        args.branches,
        instrumentation.clone()
    )?;

    if let Some(urls) = args.sub_proxies {
        for url in urls.iter() {
            let (url, freq) = parse_period(url, args.sampling_period);
            log::info!("Inserting scrape {} every {} second(s)", url, freq);
            if let Err(e) = ExporterFactory::add_scrape(factory.clone(), &url, freq) {
                log::error!("Failed add scrape : {}", e);
            }
        }
    }

    let unix = if let Some(unix) = args.unix {
        unix.clone()
    } else {
        get_proxy_path()
    };

    // Create the UNIX proxy with a reference to the exporter
    let proxy = UnixProxy::new(unix, factory.clone())?;

    // Run the proxy detached with a ref to the exporter data
    thread::spawn(move || proxy.run());

    // Start the webserver part with a reference to the exporter
    let web = Web::new(args.port, factory.clone());

    let web_url = web.url();

    if args.instrumentation > 0 {
        instrumentation.set_proxy_name(&web_url);
    }

    // Resolve the effective root proxy: CLI flag > env var > auto-discovery file > none (I am root)
    let effective_root: Option<String> = if args.root_proxy.is_some() {
        args.root_proxy.clone()
    } else if let Ok(env_root) = env::var("PROXY_ROOT_URL") {
        log::info!("Using root proxy from PROXY_ROOT_URL: {}", env_root);
        Some(env_root)
    } else if args.auto_root {
        let search_file = if let Some(dir) = &args.root_url_dir {
            dir.join("root.url")
        } else {
            root_url_file.clone()
        };
        match std::fs::read_to_string(&search_file) {
            Ok(url) => {
                let url = url.trim().to_string();
                log::info!("Auto-discovered root proxy from {}: {}", search_file.display(), url);
                Some(url)
            }
            Err(e) => {
                log::warn!("--auto-root set but could not read {}: {}", search_file.display(), e);
                None
            }
        }
    } else {
        None
    };

    // If this proxy is the root (no root resolved), publish URL for child auto-discovery
    if effective_root.is_none() {
        {
            let mut web_guard = factory.web_url.write().unwrap();
            *web_guard = Some(web_url.clone());
        }
        {
            let mut root_guard = factory.root_proxy.write().unwrap();
            *root_guard = Some(web_url.clone());
        }
        log::info!("Root proxy URL set to {}", web_url);

        // Write URL to file so child proxies with --auto_root can find us
        if let Err(e) = std::fs::write(&root_url_file, &web_url) {
            log::warn!("Could not write root URL to {}: {}", root_url_file.display(), e);
        } else {
            log::info!("Root URL written to {}", root_url_file.display());
        }
    }

    // Install graceful-leave handler: on SIGTERM/SIGINT notify root before exiting
    {
        let factory_sh = factory.clone();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        ctrlc::set_handler(move || {
            if running_clone.swap(false, Ordering::SeqCst) {
                if let (Some(root_url), Some(my_url)) = (
                    factory_sh.root_proxy.read().unwrap().clone(),
                    factory_sh.web_url.read().unwrap().clone(),
                ) {
                    // Only child proxies need to notify root (root_url != my_url)
                    if root_url != my_url {
                        let leave_url = format!("http://{}/leave?from={}", root_url, my_url);
                        log::info!("Sending graceful leave to {}", leave_url);
                        let _ = reqwest::blocking::get(&leave_url);
                    }
                }
                exit(0);
            }
        })
        .unwrap_or_else(|e| log::warn!("Failed to install signal handler: {}", e));
    }

    thread::spawn(move || {
        /* Wait for the webserver to start before joining */
        sleep(Duration::from_secs(3));
        if let Some(root) = effective_root {
            let (url, period) = parse_period(&root, args.sampling_period);

            if let Err(e) = ExporterFactory::set_data(factory.clone(), &url, &web_url, period) {
                log::error!("Failed to set data: {}", e);
                exit(1);
            }

            if let Err(e) = ExporterFactory::join(&url, &web_url, period) {
                log::error!("Failed to register in root server {}: {}", url, e);
                exit(1);
            }
        }
    });

    #[cfg(feature = "admire")]
    if args.connect_to_intelligent_controller {
        IccInterface::new(factory.clone());
    } else {
        log::info!("Not connecting to the ADMIRE intelligent controller");
    }

    #[cfg(not(feature = "admire"))]
    {
        if args.connect_to_intelligent_controller {
            unimplemented!(
                "You need to connect with the 'admire' feature enabled to connect to the ADMIRE ic"
            )
        }
    }

    web.run_blocking();

    Ok(())
}
