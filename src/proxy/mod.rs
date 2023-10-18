use std::io::Read;
use std::sync::Arc;
use std::error::Error;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::thread;

use serde_json;

use crate::proxywireprotocol::ValueDesc;

use super::proxy_common::ProxyErr;
use super::exporter::Exporter;

use super::proxywireprotocol::{ProxyCommand, ProxyCommandType};

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
	fn handle_command(exporter : Arc<Exporter>, command : ProxyCommand) ->  Result<(), Box<dyn Error>>
	{
		println!("{:?}", command);
		match command
		{
			ProxyCommand::Desc(desc) => {
				exporter.push(desc.name.as_str(), desc.doc.as_str(), desc.ctype)?;
			},
			ProxyCommand::Value(value) => {
				exporter.accumulate(value.name.as_str(), value.value)?;
			}
		}
		Ok(())
	}


	fn handle_client(exporter : Arc<Exporter>, mut stream : UnixStream) -> Result<(), Box<dyn Error>>
	{
		let mut received_data : Vec<u8> = Vec::new();

		loop {
			let mut buff : [u8; 1024] = [0; 1024];
			let len = stream.read(& mut buff)?;

         if len == 0
			{
				break;
			}

			for i in 0..len
			{
				if buff[i] == 0
				{
					/* Full command */
					let cmd : ProxyCommand = serde_json::from_slice(&received_data)?;
					UnixProxy::handle_command(exporter.clone(), cmd)?;
					received_data.clear();
				}
				else
				{
					received_data.push(buff[i]);
				}
			}
		}

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
							Ok(_) => {println!("Client left");}
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