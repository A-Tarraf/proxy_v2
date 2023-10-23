use serde::{Serialize, Deserialize};
use std::{env, collections::HashMap};

use crate::proxy_common::ProxyErr;

use super::proxy_common::unix_ts;

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[repr(u8)]
pub(crate)enum ProxyCommandType
{
	REGISTER = 0,
	SET = 1,
	GET = 2,
	LIST = 3
}



#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub(crate)enum CounterType
{
	COUNTER = 0,
	GAUGE = 1
}

#[derive(Serialize,Deserialize, Debug)]
pub(crate)struct ValueDesc
{
	pub(crate)name : String,
	pub(crate)doc : String,
	pub(crate)ctype : CounterType
}

#[derive(Serialize, Deserialize, Debug)]
pub(crate)struct CounterValue
{
	pub(crate)name : String,
	pub(crate)value : f64
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate)struct JobDesc
{
	pub(crate)jobid : String,
	pub(crate)command : String,
	pub(crate)size : i32,
	pub(crate)nodelist : String,
	pub(crate)partition : String,
	pub(crate)cluster : String,
	pub(crate)run_dir : String,
	pub(crate)start_time : u64,
	pub(crate)end_time : u64
}


impl JobDesc
{
	pub fn merge(&mut self, other_desc : JobDesc) -> Result<(), ProxyErr>
	{
		/* First handle descs */
		if self.jobid != other_desc.jobid
		{
			return Err(ProxyErr::new("Mismatching job ids"))
		}

		if self.size != other_desc.size
		{
			return Err(ProxyErr::new("Mismatching sizes id"))
		}

		if let Some(min) = [self.start_time, other_desc.start_time].iter().min().cloned()
		{
			self.start_time = min;
		}

		if let Some(max) = [self.end_time, other_desc.end_time].iter().max().cloned()
		{
			self.end_time = max;
		}

		Ok(())
	}
}

impl JobDesc
{
	// Only used in the client library
	#[allow(unused)]
	pub(crate)fn new() -> JobDesc
	{
		let mut jobid = env::var("PROXY_JOB_ID")
		.or_else(|_| env::var("SLURM_JOBID"))
		.or_else(|_| env::var("PMIX_ID"))
		.unwrap_or_else(|_| "".to_string());

		/* Concatenate the step id if present  */
		if let Ok(stepid) = env::var("SLURM_STEP_ID")
		{
			jobid += format!("-{}", stepid).as_str();
		}

		/* Remove the rank at the end from the PMIx JOBID */
		if jobid.contains('.')
		{
			let no_rank : Vec<&str> = jobid.split('.').collect();
			jobid = no_rank[0].to_string();
		}

		let size = env::var("SLURM_NTASKS")
		.or_else(|_| env::var("OMPI_COMM_WORLD_SIZE"))
		.unwrap_or("1".to_string())
		.parse::<i32>()
		.unwrap_or(1);

		let nodelist = env::var("SLURM_JOB_NODELIST").unwrap_or("".to_string());
		let partition = env::var("SLURM_JOB_PARTITION").unwrap_or("".to_string());
		let cluster = env::var("SLURM_CLUSTER_NAME").unwrap_or("".to_string());
		let run_dir = env::current_dir().map(|v| v.to_string_lossy().to_string()).unwrap_or("".to_string());

		let cmdline_bytes = std::fs::read("/proc/self/cmdline").unwrap_or(Vec::new());
		let command = String::from_utf8(cmdline_bytes).unwrap_or("".to_string());
		let command = command.replace('\0', " ");

		JobDesc { jobid,
					 command,
					 size,
					 nodelist,
					 partition,
					 cluster,
					 run_dir,
					 start_time: unix_ts(),
					 end_time: 0
					}
	}
}

#[derive(Serialize,Deserialize, Debug)]
pub(crate)enum ProxyCommand
{
	Desc(ValueDesc),
	Value(CounterValue),
	JobDesc(JobDesc)
}

#[derive(Serialize,Deserialize, Debug, Clone)]
pub(crate)struct CounterSnapshot
{
	pub(crate)name : String,
	pub(crate)doc : String,
	pub(crate)ctype : CounterType,
	pub(crate)value : f64
}

#[derive(Serialize,Deserialize, Debug, Clone)]
pub(crate)struct JobProfile
{
	pub(crate)desc : JobDesc,
	pub(crate)counters : Vec<CounterSnapshot>
}

impl JobProfile
{
	pub(crate) fn merge(&mut self, other_prof : JobProfile) -> Result<(), ProxyErr>
	{
		self.desc.merge(other_prof.desc)?;

		/* Map all counters from self */
		let mut map : HashMap<String, CounterSnapshot> = self.counters.iter().map(|v| (v.name.to_string(), v.clone())).collect();

		for cnt in other_prof.counters.iter()
		{
			if let Some(existing) = map.get_mut(&cnt.name)
			{
				existing.value += cnt.value;
			}
			else
			{
				map.insert(cnt.name.to_string(), cnt.clone());
			}
		}

		self.counters = map.values().into_iter().cloned().collect();

		Ok(())
	}

	pub (crate) fn substract(&mut self, previous : & JobProfile) -> Result<(), ProxyErr>
	{
		/* Map all counters from self */
		let mut map : HashMap<String, CounterSnapshot> = self.counters.iter().map(|v| (v.name.to_string(), v.clone())).collect();

		for cnt in previous.counters.iter()
		{
			if let Some(existing) = map.get_mut(&cnt.name)
			{
				if existing.value < cnt.value
				{
					log::error!("Cannot substract non-monothonic counter {}", existing.name);
					return Err(ProxyErr::new("Non monothonic substraction attempted"));
				}
				existing.value -= cnt.value;
			}
			else
			{
				map.insert(cnt.name.to_string(), cnt.clone());
			}
		}

		self.counters = map.values().into_iter().cloned().collect();

		Ok(())
	}
}