use std::error::Error;

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
	 pub(crate) fn new(message: &str) -> ProxyErr {
		ProxyErr {
				message: message.to_string(),
		}
 
	 }
 }
 
 impl std::fmt::Display for ProxyErr {
	 fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		  write!(f, "{}", self.message)
	 }
 }