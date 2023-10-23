use std::error::Error;
use std::thread;
mod proxy_common;
use proxy_common::init_log;

mod exporter;
use exporter::ExporterFactory;

mod proxy;
use proxy::UnixProxy;

mod webserver;
use webserver::Web;

mod proxywireprotocol;

use dirs;

extern crate clap;

use clap::Parser;

/// ADMIRE project Instrumentation Proxy
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    // Port number of the HTTP server
    #[arg(short, long, default_value_t = 1337)]
    port: u32,

	// Path of the UNIX proxy for the gateway
	#[arg(short, long, default_value = "/tmp/metric_proxy.unix")]
	unix : String,

    /// Should profile aggregation be deactivated
    #[arg(short, long, default_value_t = false)]
    inhibit_profile_agreggation: bool,


    /// Subservers to be scrapped (optionnal comma separated list)
    #[arg(short, long, value_delimiter = ',')]
    sub_proxies: Option<Vec<String>>,
}


fn main() -> Result<(), Box<dyn Error>>
{
	init_log();

	let args = Args::parse();

	let mut profile_prefix = dirs::home_dir().unwrap();
	profile_prefix.push(".proxyprofiles");


	// The central storage is the exporter
	let factory = ExporterFactory::new(profile_prefix, !args.inhibit_profile_agreggation);

	if let Some(urls) = args.sub_proxies
	{
		for url in urls.iter()
		{
			ExporterFactory::add_scrape(factory.clone(), url, 10);
		}
	}

	// Create the UNIX proxy with a reference to the exporter
	let proxy = UnixProxy::new(args.unix, factory.clone())?;

	// Run the proxy detached with a ref to the exporter data
	thread::spawn(move || {
		proxy.run()
	});

	// Start the webserver part with a reference to the exporter
	let web = Web::new(args.port, factory.clone());
	web.run_blocking();

	Ok(())
}
