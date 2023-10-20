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


fn main() -> Result<(), Box<dyn Error>>
{
	init_log();


	let mut profile_prefix = dirs::home_dir().unwrap();
	profile_prefix.push(".proxyprofiles");


	// The central storage is the exporter
	let factory = ExporterFactory::new(profile_prefix, true);

	// Create the UNIX proxy with a reference to the exporter
	let proxy = UnixProxy::new("/tmp/test_sock".to_string(), factory.clone())?;

	// Run the proxy detached with a ref to the exporter data
	thread::spawn(move || {
		proxy.run()
	});

	// Start the webserver part with a reference to the exporter
	let web = Web::new(1337, factory.clone());
	web.run_blocking();

	Ok(())
}
