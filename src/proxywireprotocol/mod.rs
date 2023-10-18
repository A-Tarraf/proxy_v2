use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[repr(u8)]
pub enum ProxyCommandType
{
	REGISTER = 0,
	SET = 1,
	GET = 2,
	LIST = 3
}



#[derive(Serialize, Deserialize, PartialEq, Debug)]
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

#[derive(Serialize, Deserialize, Debug)]
pub struct JobDesc
{
	pub njobid : String,
	pub command : String,
	pub size : i32,
	pub nodelist : String,
	pub partition : String,
	pub cluster : String,
	pub run_dir : String,
	pub start_time : u64,
	pub end_time : u64
}

#[derive(Serialize,Deserialize, Debug)]
pub enum ProxyCommand
{
	Desc(ValueDesc),
	Value(CounterValue),
	JobDesc(JobDesc)
}
