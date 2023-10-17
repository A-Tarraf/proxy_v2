use std::collections::HashMap;
use std::sync::{RwLock, Arc};
use super::proxy_common::ProxyErr;

/***********************
 * PROMETHEUS EXPORTER *
 ***********************/
 struct ExporterCounter {
	name: String,
	value: Arc<
				RwLock<
						f64
						>
				>
}

impl ExporterCounter
{
	fn new(name : String, value : f64) -> ExporterCounter
	{
		ExporterCounter{
			name,
			value : Arc::new(RwLock::new(value))
		}
	}
}


struct ExporterCounterGroup {
	basename : String,
	doc: String,
	ht: RwLock<
					HashMap<String, ExporterCounter>
				>
}

impl ExporterCounterGroup
{
	fn new(basename : String, doc : String) -> ExporterCounterGroup
	{
		ExporterCounterGroup{
			basename,
			doc,
			ht : RwLock::new(HashMap::new())
		}
	}

	fn basename(name : String ) -> String
	{
		let spl : Vec<&str> = name.split("{").collect();
		spl[0].to_string()
	}

	fn set(& self, name : &str, value : f64) -> Result<(), ProxyErr>
	{
		match self.ht.write().unwrap().get_mut(name)
		{
			Some(v) => {
				let mut val = v.value.write().unwrap();
				*val = value;
				return Ok(());
			}
			None => {
				return Err(ProxyErr::new("Failed to set counter"));
			}
		}
	}

	fn inc(& self, name : &str, value : f64) -> Result<(), ProxyErr>
	{
		match self.ht.write().unwrap().get_mut(name)
		{
			Some(v) => {
				let mut val = v.value.write().unwrap();
				*val += value;
				return Ok(());
			}
			None => {
				return Err(ProxyErr::new("Failed to set counter"));
			}
		}
	}

	fn push(& self, name : &str, value : f64) -> Result<(), ProxyErr>
	{
		if self.ht.read().unwrap().contains_key(name)
		{
			return self.inc(name, value);
		}
		else
		{
			if name.contains("{")
			{
				if ! name.contains("}")
				{
					return Err(ProxyErr::new(format!("Bad metric name '{}' unmatched brackets",name.to_string()).as_str()));
				}
			}
			let new = ExporterCounter::new(name.to_string(), value);
			self.ht.write().unwrap().insert(name.to_string(), new);
		}

		Ok(())
	}

	fn serialize(& self ) -> Result<String, ProxyErr>
	{
		let mut ret: String = String::new();

		ret += format!("# HELP {} {}\n", self.basename, self.doc).as_str();
		ret += format!("# TYPE {} counter\n", self.basename).as_str();

		for (_, exporter_counter) in self.ht.read().unwrap().iter() {
			 // Acquire the Mutex for this specific ExporterCounter
			 let value = exporter_counter.value.read().unwrap();
			 ret += format!("{} {}\n", exporter_counter.name, value).as_str();
		}

		Ok(ret)
	}
}



pub(crate) struct Exporter {
	ht: RwLock<
					HashMap<String, ExporterCounterGroup>
				>
}

impl Exporter {
	pub(crate) fn new() -> Exporter {
		 Exporter {
			  ht: RwLock::new(HashMap::new()),
		 }
	}

	pub(crate) fn inc(&self, name: &str, value: f64) -> Result<(), ProxyErr> {
		let basename = ExporterCounterGroup::basename(name.to_string());

		 if let Some(exporter_counter) = self.ht.read().unwrap().get(basename.as_str())
		 {
			return exporter_counter.inc(name, value);
		 }
		 else
		 {
			return Err(ProxyErr::new(format!("No such key {} cannot set it", name).as_str()));
		 }
	}

	pub(crate) fn set(&self, name: &str, value: f64) -> Result<(), ProxyErr> {
		let basename = ExporterCounterGroup::basename(name.to_string());
		
		if let Some(exporter_counter) = self.ht.read().unwrap().get(basename.as_str())
		{
			return exporter_counter.set(name, value);
		}
		else
		{
		  return Err(ProxyErr::new(format!("No such key {} cannot set it", name).as_str()));
		}
  }

	pub(crate) fn push(&self, name: &str, doc: &str, value: f64) -> Result<(), ProxyErr> {
		let basename = ExporterCounterGroup::basename(name.to_string());

		let mut ht = self.ht.write().unwrap();
	
		if let Some(exporter_counter) = ht.get(basename.as_str())
		{
			return exporter_counter.push(name, value);
		}
		else
		{
			let ncnt = ExporterCounterGroup::new(basename.to_owned(), doc.to_string());
			ncnt.push(name, value)?;
			ht.insert(basename, ncnt);
		 }

		 Ok(())
	}

	pub(crate) fn serialize(&self) -> Result<String, ProxyErr> {
		 let mut ret: String = String::new();

		 for (_, exporter_counter) in self.ht.read().unwrap().iter() {
			  ret += exporter_counter.serialize()?.as_str();
		 }

		 ret += "# EOF\n";

		 Ok(ret)
	}
}