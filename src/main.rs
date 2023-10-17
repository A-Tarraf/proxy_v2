use std::sync::Arc;
use std::error::Error;
use std::thread;
mod proxy_common;

mod exporter;
use exporter::Exporter;

mod proxy;
use proxy::UnixProxy;

mod webserver;
use webserver::Web;

mod proxywireprotocol;


fn main() -> Result<(), Box<dyn Error>>
{
	// The central storage is the exporter
	let exporter = Arc::new(Exporter::new());

	// Create the UNIX proxy with a reference to the exporter
	let proxy = UnixProxy::new("/tmp/test_sock".to_string(), exporter.clone())?;

	// Run the proxy detached with a ref to the exporter data
	thread::spawn(move || {
		proxy.run()
	});

	// Start the webserver part with a reference to the exporter
	let web = Web::new(1337, exporter.clone());
	web.run_blocking();

	Ok(())
}
