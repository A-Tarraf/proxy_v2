use std::error::Error;
use env_logger;
use std::time::{SystemTime, UNIX_EPOCH};

/*******************
 * IMPLEMENT ERROR *
 *******************/

#[derive(Debug)]
pub(crate) struct ProxyErr
{
	message : String,
}

impl Error for ProxyErr {}

impl ProxyErr {
	// Create a constructor method for your custom error
	#[allow(unused)]
	pub(crate) fn new(message: &str) -> ProxyErr {
	ProxyErr {
			message: message.to_string(),
	}
	}

	#[allow(unused)]
	pub(crate) fn newboxed(message: &str) -> Box<ProxyErr> {
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

pub fn init_log()
{
	let env = env_logger::Env::new()
							.default_filter_or("info");
	env_logger::init_from_env(env);
}

#[allow(unused)]
pub fn unix_ts() -> u64
{
	let current_time = SystemTime::now();
	current_time.duration_since(UNIX_EPOCH).expect("Time went backwards").as_secs()
}

#[allow(unused)]
pub fn unix_ts_us() -> u128
{
	let current_time = SystemTime::now();
	current_time.duration_since(UNIX_EPOCH).expect("Time went backwards").as_micros()
}