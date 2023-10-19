use serde::{Serialize, Deserialize};
use std::env;

use super::proxy_common::unix_ts;

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[repr(u8)]
pub enum ProxyCommandType
{
	REGISTER = 0,
	SET = 1,
	GET = 2,
	LIST = 3
}



#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
#[repr(u8)]
pub enum CounterType
{
	COUNTER = 0,
	GAUGE = 1
}

#[derive(Serialize,Deserialize, Debug)]
pub struct ValueDesc
{
	pub name : String,
	pub doc : String,
	pub ctype : CounterType
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CounterValue
{
	pub name : String,
	pub value : f64
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JobDesc
{
	pub jobid : String,
	pub command : String,
	pub size : i32,
	pub nodelist : String,
	pub partition : String,
	pub cluster : String,
	pub run_dir : String,
	pub start_time : u64,
	pub end_time : u64
}

impl JobDesc
{
	// Only used in the client library
	#[allow(unused)]
	pub fn new() -> JobDesc
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
pub enum ProxyCommand
{
	Desc(ValueDesc),
	Value(CounterValue),
	JobDesc(JobDesc)
}

#[derive(Serialize,Deserialize, Debug)]
pub struct CounterSnapshot
{
	pub name : String,
	pub doc : String,
	pub ctype : CounterType,
	pub value : f64
}

#[derive(Serialize,Deserialize, Debug)]
pub struct JobProfile
{
	pub desc : JobDesc,
	pub counters : Vec<CounterSnapshot>
}