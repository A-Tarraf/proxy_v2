use rouille::{Response, Request};
use std::thread::JoinHandle;
use std::error::Error;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;

/*******************
 * IMPLEMENT ERROR *
 *******************/

#[derive(Debug)]
struct ProxyErr
{
	message : String,
}

impl Error for ProxyErr {}

impl ProxyErr {
	// Create a constructor method for your custom error
	fn new(message: &str) -> ProxyErr {
		ProxyErr {
			  message: message.to_string(),
		 }

	}
}

impl std::fmt::Display for ProxyErr {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		 write!(f, "Custom error: {}", self.message)
	}
}

/***********************
 * PROMETHEUS EXPORTER *
 ***********************/

struct ExporterCounter
{
	name : String,
	doc :String,
	value : f64
}


struct Exporter
{
	ht : HashMap<String, ExporterCounter>
}


impl Exporter
{
	fn new() -> Exporter
	{
		Exporter{
			ht : HashMap::new()
		}
	}

	fn set(& mut self, name : & str, value : f64) -> Result<(), ProxyErr>
	{
		let c = self.ht.get_mut(&name.to_string());
		match c
		{
			None => {
				return Err(ProxyErr::new(format!("Failed to set key {}",name).as_str()));
			}
			Some(e) => {
				e.value = value;
			}
		}
		Ok(())
	}

	fn add(& mut self, name : & str, doc : & str, value : f64) -> Result<(), ProxyErr>
	{
		if self.ht.contains_key(&name.to_string())
		{
			return self.set(name, value);
		}
		else
		{
			let ncnt = ExporterCounter{
				 name : name.to_string(),
				 doc: doc.to_string(),
				 value : value
			};

			self.ht.insert(name.to_string(), ncnt);
		}

		Ok(())
	}

	fn serialize(& self) -> Result<String, ProxyErr>
	{
		let mut ret : String = String::new();
		for (_, v) in self.ht.iter()
		{
			ret += format!("# HELP {} {}\n", v.name, v.doc).as_str();
			ret += format!("# TYPE {} counter\n", v.name).as_str();
			ret += format!("{} = {}\n", v.name, v.value).as_str();
		}

		ret += "# EOF\n";

		Ok(ret)
	}

}





/*************
 * WEBSERVER *
 *************/
struct Web
{
	port : u32,
}

#[derive(Serialize)]
struct ApiResponse
{
	operation : String,
	success : bool
}


enum WebResponse {
	HTML(String),
	Text(String),
	BadReq(String),
	Success(String),
	NoSuchDoc()
}

impl WebResponse
{
	fn serialize(self : WebResponse) -> Response
	{
		match self
		{
			WebResponse::HTML(s) => 
			{
				Response::text(s)
			}
			WebResponse::Text(s) => 
			{
				Response::html(s)
			}
			WebResponse::BadReq(operation) => 
			{
				let r = ApiResponse{
					operation,
					success : false
				};
				Response::json(&r).with_status_code(400)
			}
			WebResponse::Success(operation) =>
			{
				let r = ApiResponse{
					operation,
					success : true
				};
				Response::json(&r)
			}
			WebResponse::NoSuchDoc() => 
			{
				Response::empty_404()
			}
		}
	}
}



impl Web
{
	fn new(port : u32) -> Web
	{
		Web { port: port }
	}

	fn handle_set(&self, req : &Request) -> WebResponse
	{
		let key : Option<String>;
		let value : Option<String>;

		#[derive(Deserialize)]
		struct KeyValue
		{
			key : String,
			value : String
		}

		match req.method()
		{
			"GET" =>
			{
				key = req.get_param("key");
				value = req.get_param("value");
			}
			"POST" =>
			{
				let kv : Result<KeyValue, rouille::input::json::JsonError> = rouille::input::json_input(req);
				match kv
				{
					Ok(v) => {
						key = Some(v.key);
						value = Some(v.value);
					}
					Err(e) => {
						return WebResponse::BadReq(e.to_string())
					}
				}
			}
			_ => {
				return WebResponse::BadReq(format!("Unknown request method type {}", req.method()))
			}
		}

		if key.is_none() || value.is_none()
		{
			return WebResponse::BadReq("missing parameter".to_string())
		}

		WebResponse::Success("set".to_string())
	}


	fn run_blocking(self)
	{
		rouille::start_server(format!("0.0.0.0:{}",self.port), move | request| {

			let resp : WebResponse;

			match request.url().as_str()
			{
				"/set" => {
					resp = self.handle_set(&request);
				},
				_ => {
					resp = WebResponse::NoSuchDoc();
				}
			}

			resp.serialize()
	  });
	}
}



/***************
 * THREAD JOIN *
 ***************/

fn join_thread<T>(a : JoinHandle<T>) -> Result<(), ProxyErr>
{
	match a.join()
	{
		Ok(_) =>
		{
			Ok(())
		}
		Err(_) => { 
			Err(ProxyErr::new("Failed to join thread"))
		}
	}
}


fn main() -> Result<(), Box<dyn Error>>
{

	let mut exporter = Exporter::new();

	exporter.add("test_total", "Test value", 0.0)?;

	print!("{}" , exporter.serialize()?);

	exporter.set("test_total", 10.0)?;


	print!("{}" , exporter.serialize()?);

	exporter.set("test_total", 10.0)?;


	print!("{}" , exporter.serialize()?);



	let web = Web::new(8080);

	web.run_blocking();

	Ok(())
}
