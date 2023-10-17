use std::ptr::null;
use std::{error::Error, io::Write};
use std::os::unix::net::UnixStream;
use std::ffi::CStr;
use std::path::Path;

use std::ffi::c_void;
use serde_json;

use std::env;

mod proxy_common;
use proxy_common::ProxyErr;

mod proxywireprotocol;
use proxywireprotocol::{ProxyCommand, CounterType, ValueDesc};



struct MetricProxyClient
{
	running : bool,
	stream : Option<UnixStream>
}


impl MetricProxyClient
{
	fn new() -> MetricProxyClient
	{
		let mut can_run : bool = true;
		
		let sock_path = env::var("PROXY_PATH").unwrap_or("/tmp/metric_proxy".to_string());
		let path = Path::new(&sock_path);

		if !path.exists()
		{
			can_run = false;
		}

		let tsock : Option<UnixStream>;

		if can_run
		{
			tsock = UnixStream::connect(path).ok();
		}
		else
		{
			tsock = None;
		}

		let client = MetricProxyClient{
			running : can_run,
			stream : tsock
		};
	
		return client;
	}

	fn send(&mut self, cmd : &ProxyCommand) -> Result<(), Box<dyn Error>>
	{
		if self.stream.is_none()
		{
			self.running = false;
			return Err(Box::new(ProxyErr::new("Not connected to UNIX socket")));
		}

		let mut stream =  self.stream.take().unwrap();

		serde_json::to_writer( &mut stream, cmd)?;
		let null_byte : [u8; 1] = [0; 1];
		stream.write_all(&null_byte)?;


		println!("{:?}", cmd);

		/* Restore the value */
		self.stream = Some(stream);

		Ok(())
	}

	fn new_counter(&mut self, name : String, doc : String) -> Result<(), Box<dyn Error>>
	{
		let command = ProxyCommand::Desc(ValueDesc {
			name,
			doc,
			ctype : CounterType::COUNTER
		});

		self.send(&command)?;


		Ok(())
	}


}


#[no_mangle]
pub extern "C" fn metric_proxy_init() -> *mut c_void
{
	let client = Box::new(MetricProxyClient::new());
	return Box::into_raw(client) as *mut c_void;
}

#[no_mangle]
pub extern "C" fn metric_proxy_release(pclient : *mut c_void) -> std::ffi::c_int
{
	/* Get the client back and drop it */
	let _ : Box<MetricProxyClient> = unsafe {Box::from_raw(pclient as *mut MetricProxyClient)};
	let zero: std::ffi::c_int = 0.into();
	return zero;
}



fn unwrap_c_string(pcstr: *const std::os::raw::c_char) -> Result<String, Box<dyn Error>>
{
	// Convert the `char*` to a Rust CStr
	let cstr = unsafe { CStr::from_ptr(pcstr) };

	// Convert the CStr to a Rust &str (Unicode)
	match cstr.to_str()
	{
		Ok(e) => {
			return Ok(e.to_string());
		}
		Err(e) => {
			return Err(Box::new(e));
		}
	}
}


#[no_mangle]
pub extern "C" fn metric_proxy_new_counter(pclient : *mut c_void,
														 name: *const std::os::raw::c_char,
									  					 doc : *const std::os::raw::c_char) -> *mut c_void
{

	let rname = unwrap_c_string(name);
	let rdoc = unwrap_c_string(doc);

	if rname.is_err() || rdoc.is_err() || pclient.is_null()
	{
		return std::ptr::null_mut();
	}

	let client: &mut MetricProxyClient = unsafe { &mut *(pclient as *mut MetricProxyClient) };

	if !client.running
	{
		return std::ptr::null_mut();
	}

	let rname = rname.unwrap();
	let rdoc = rdoc.unwrap();

	if let Ok(_c) = client.new_counter(rname, rdoc)
	{
		return std::ptr::null_mut();
	}

	return std::ptr::null_mut();
}