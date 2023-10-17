use core::borrow;
use std::io::Read;
use std::sync::Arc;
use std::error::Error;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::{thread, mem};


use super::proxy_common::ProxyErr;
use super::exporter::Exporter;

/********************
 * UNIX DATA SERVER *
 ********************/

 pub(crate) struct UnixProxy
 {
	listener : UnixListener,
	socket_path : String,
	exporter : Arc<Exporter>
}






impl UnixProxy
{


	fn handle_client(exporter : Arc<Exporter>, mut stream : UnixStream) -> Result<(), Box<dyn Error>>
	{
			Ok(())
	}

	pub(crate) fn run(&self) -> Result<(), ProxyErr>
	{
		for stream in self.listener.incoming() {
			match stream {
				Ok(stream) => {
					println!("New connection");

					let exporter = self.exporter.clone();

					// Handle the connection in a new thread.
					thread::spawn(move || {
						match UnixProxy::handle_client(exporter, stream) {
							Ok(_) => {return;}
							Err(e) => {println!("Proxy server closing on client : {}", e.to_string());}
						}
					});
				}
				Err(err) => {
					eprintln!("Error accepting connection: {:?}", err);
				}
			}
		}

	  Ok(())
	}

	pub(crate) fn new(socket_path : String, exporter : Arc<Exporter>) -> Result<UnixProxy, Box<dyn Error>>
	{
		let path = Path::new(&socket_path);

		if path.exists()
		{
			std::fs::remove_file(path)?;
		}

		let listener = UnixListener::bind(path)?;

		let proxy = UnixProxy{
			listener,
			socket_path,
			exporter
		};

		Ok(proxy)
	}
 }