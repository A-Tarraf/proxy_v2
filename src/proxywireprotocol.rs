use crate::proxy_common::unix_ts;
use crate::proxy_common::ProxyErr;

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::{Arc, RwLock};

use std::{collections::HashMap, env, error::Error};

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum CounterType {
    Counter {
        value: f64,
    },
    Gauge {
        min: f64,
        max: f64,
        hits: f64,
        total: f64,
    },
}

impl fmt::Display for CounterType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CounterType::Counter { value } => {
                write!(f, "{} COUNTER", value)
            }
            CounterType::Gauge {
                min,
                max,
                hits,
                total,
            } => {
                write!(
                    f,
                    "{} (Min: {}, Max : {}, Hits: {}, Total : {}) GAUGE",
                    total / hits,
                    min,
                    max,
                    hits,
                    total
                )
            }
        }
    }
}

impl CounterType {
    pub fn newcounter() -> CounterType {
        Self::Counter { value: 0.0 }
    }

    #[allow(unused)]
    pub fn newgauge() -> CounterType {
        Self::Gauge {
            min: 0.0,
            max: 0.0,
            hits: 0.0,
            total: 0.0,
        }
    }

    #[allow(unused)]
    pub fn hasdata(&self) -> bool {
        match self {
            CounterType::Counter { value } => *value != 0.0,
            Self::Gauge {
                min: _,
                max: _,
                hits,
                total: _,
            } => *hits != 0.0,
        }
    }

    #[allow(unused)]
    fn value(&self) -> f64 {
        match self {
            Self::Counter { value } => *value,
            Self::Gauge {
                min: _,
                max: _,
                hits,
                total,
            } => *total / *hits,
        }
    }

    fn serialize(&self, name: &String) -> String {
        match self {
            Self::Counter { value } => {
                format!("{} {}\n", name, value)
            }
            Self::Gauge {
                min: _,
                max: _,
                hits,
                total,
            } => {
                format!("{} {}\n", name, total / hits,)
            }
        }
    }

    pub(crate) fn merge(&mut self, other: &CounterType) -> Result<(), ProxyErr> {
        self.same_type(other)?;
        match other {
            CounterType::Counter { value } => {
                /* For a counter we simply add the local and remote values */
                match self {
                    CounterType::Counter { value: svalue } => {
                        *svalue += *value;
                        Ok(())
                    }
                    _ => unreachable!(),
                }
            }
            CounterType::Gauge {
                min,
                max,
                hits,
                total,
            } => {
                /* Here we sum the values and keep min and max accordingly */
                match self {
                    CounterType::Gauge {
                        min: smin,
                        max: smax,
                        hits: shits,
                        total: stotal,
                    } => {
                        *smin = min_f64(*smin, *min);
                        *smax = max_f64(*smax, *max);
                        *shits += hits;
                        *stotal += total;
                        Ok(())
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    #[allow(unused)]
    pub(crate) fn set(&mut self, other: &CounterType) -> Result<(), ProxyErr> {
        self.same_type(other)?;
        match other {
            CounterType::Counter { value } => {
                /* For a counter we simply add the local and remote values */
                match self {
                    CounterType::Counter { value: svalue } => {
                        *svalue += *value;
                        Ok(())
                    }
                    _ => unreachable!(),
                }
            }
            CounterType::Gauge {
                min: _,
                max: _,
                hits: _,
                total,
            } => {
                /* Here we sum the values and keep min and max accordingly */
                match self {
                    CounterType::Gauge {
                        min: smin,
                        max: smax,
                        hits: shits,
                        total: stotal,
                    } => {
                        *smin = *total;
                        *smax = *total;
                        *shits = 1.0;
                        *stotal = *total;
                        Ok(())
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    fn delta(&mut self, other: &CounterType) -> Result<(), ProxyErr> {
        self.same_type(other)?;
        match other {
            CounterType::Counter { value } => {
                /* For a counter we simply add the local and remote values */
                match self {
                    CounterType::Counter { value: svalue } => {
                        *svalue -= *value;
                        Ok(())
                    }
                    _ => unreachable!(),
                }
            }
            CounterType::Gauge {
                min,
                max,
                hits,
                total,
            } => {
                /* Here we sum the values and keep min and max accordingly */
                match self {
                    CounterType::Gauge {
                        min: smin,
                        max: smax,
                        hits: shits,
                        total: stotal,
                    } => {
                        *smin = min_f64(*smin, *min);
                        *smax = max_f64(*smax, *max);
                        *shits -= hits;
                        *stotal -= total;
                        Ok(())
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    fn same_type(&self, other: &CounterType) -> Result<(), ProxyErr> {
        match (&self, &other) {
            (CounterType::Gauge { .. }, CounterType::Gauge { .. }) => Ok(()),
            (CounterType::Counter { .. }, CounterType::Counter { .. }) => Ok(()),
            _ => Err(ProxyErr::new(format!(
                "Both instances are not of the same variant {:?} and {:?}",
                self, other
            ))),
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub(crate) enum AlarmOperator {
    #[allow(unused)]
    Equal(f64),
    #[allow(unused)]
    Less(f64),
    #[allow(unused)]
    More(f64),
}

impl AlarmOperator {
    fn apply(&self, val: &CounterType) -> bool {
        let value: f64 = val.value();

        match self {
            Self::Equal(v) => *v == value,
            Self::Less(v) => *v > value,
            Self::More(v) => *v < value,
        }
    }
}

impl fmt::Display for AlarmOperator {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self {
            Self::Equal(v) => write!(f, "= {}", *v),
            Self::Less(v) => write!(f, "< {}", *v),
            Self::More(v) => write!(f, "> {}", *v),
        }
    }
}

#[derive(Serialize, Debug)]
pub(crate) struct ValueAlarmTrigger {
    pub(crate) name: String,
    pub(crate) metric: String,
    pub(crate) operator: AlarmOperator,
    pub(crate) current: f64,
    pub(crate) active: bool,
    pub(crate) pretty: String,
}

pub(crate) struct ValueAlarm {
    name: String,
    counter: Arc<RwLock<CounterSnapshot>>,
    op: AlarmOperator,
}

impl fmt::Display for ValueAlarm {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} : {} {}",
            self.name,
            self.counter.read().unwrap(),
            self.op
        )
    }
}

impl ValueAlarm {
    #[allow(unused)]
    pub(crate) fn new(
        name: &String,
        counter: Arc<RwLock<CounterSnapshot>>,
        op: String,
        val: f64,
    ) -> Result<ValueAlarm, ProxyErr> {
        let alop = match op.as_str() {
            "=" => AlarmOperator::Equal(val),
            "<" => AlarmOperator::Less(val),
            ">" => AlarmOperator::More(val),
            _ => {
                return Err(ProxyErr::new(format!(
                    "No operator for {} only has = < and >",
                    op
                )));
            }
        };

        Ok(ValueAlarm {
            name: name.to_string(),
            counter: counter.clone(),
            op: alop,
        })
    }

    #[allow(unused)]
    pub(crate) fn as_trigger(&self, active: Option<bool>) -> ValueAlarmTrigger {
        let cnt_locked = self.counter.read().unwrap();

        let is_active = match active {
            Some(v) => v,
            None => self.op.apply(&self.counter.read().unwrap().ctype),
        };

        ValueAlarmTrigger {
            name: self.name.to_string(),
            metric: cnt_locked.name.to_string(),
            operator: self.op.clone(),
            current: cnt_locked.ctype.value(),
            active: is_active,
            pretty: self.to_string(),
        }
    }

    #[allow(unused)]
    pub(crate) fn check(&self) -> Option<ValueAlarmTrigger> {
        if self.op.apply(&self.counter.read().unwrap().ctype) {
            Some(self.as_trigger(Some(true)))
        } else {
            None
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub(crate) struct ValueDesc {
    pub(crate) name: String,
    pub(crate) doc: String,
    pub(crate) ctype: CounterType,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct CounterValue {
    pub(crate) name: String,
    pub(crate) value: CounterType,
}

impl CounterValue {
    #[allow(unused)]
    pub fn reset(&mut self) {
        self.value = match self.value {
            CounterType::Counter { value } => CounterType::Counter { value: 0.0 },
            CounterType::Gauge {
                min,
                max,
                hits: _,
                total: _,
            } => CounterType::Gauge {
                min,
                max,
                hits: 0.0,
                total: 0.0,
            },
        };
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct JobDesc {
    pub(crate) jobid: String,
    pub(crate) command: String,
    pub(crate) size: i32,
    pub(crate) nodelist: String,
    pub(crate) partition: String,
    pub(crate) cluster: String,
    pub(crate) run_dir: String,
    pub(crate) start_time: u64,
    pub(crate) end_time: u64,
}

impl JobDesc {
    pub fn merge(&mut self, other_desc: JobDesc) -> Result<(), ProxyErr> {
        /* First handle descs */
        if self.jobid != other_desc.jobid {
            return Err(ProxyErr::new("Mismatching job ids"));
        }

        if self.size != other_desc.size {
            return Err(ProxyErr::new("Mismatching sizes id"));
        }

        if let Some(min) = [self.start_time, other_desc.start_time]
            .iter()
            .min()
            .cloned()
        {
            self.start_time = min;
        }

        if let Some(max) = [self.end_time, other_desc.end_time].iter().max().cloned() {
            self.end_time = max;
        }

        Ok(())
    }
}

impl JobDesc {
    // Only used in the client library
    #[allow(unused)]
    pub(crate) fn new() -> JobDesc {
        let mut jobid = env::var("PROXY_JOB_ID")
            .or_else(|_| env::var("SLURM_JOBID"))
            .or_else(|_| env::var("PMIX_ID"))
            .unwrap_or_else(|_| "".to_string());

        /* Concatenate the step id if present  */
        if let Ok(stepid) = env::var("SLURM_STEP_ID") {
            jobid += format!("-{}", stepid).as_str();
        }

        /* Remove the rank at the end from the PMIx JOBID */
        if jobid.contains('.') {
            let no_rank: Vec<&str> = jobid.split('.').collect();
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
        let run_dir = env::current_dir()
            .map(|v| v.to_string_lossy().to_string())
            .unwrap_or("".to_string());

        let cmdline_bytes = std::fs::read("/proc/self/cmdline").unwrap_or(Vec::new());
        let command = String::from_utf8(cmdline_bytes).unwrap_or("".to_string());
        let command = command.replace('\0', " ");
        let command = command
            .find("--")
            .map(|index| &command[(index + "--".len())..])
            .unwrap_or(command.as_str())
            .to_string();

        JobDesc {
            jobid,
            command,
            size,
            nodelist,
            partition,
            cluster,
            run_dir,
            start_time: unix_ts(),
            end_time: 0,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub(crate) enum ProxyCommand {
    Desc(ValueDesc),
    Value(CounterValue),
    JobDesc(JobDesc),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct CounterSnapshot {
    pub(crate) name: String,
    pub(crate) doc: String,
    pub(crate) ctype: CounterType,
}

impl fmt::Display for CounterSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} ({}) = {}", self.name, self.doc, self.ctype)
    }
}

pub fn min_f64(a: f64, b: f64) -> f64 {
    if a < b {
        a
    } else {
        b
    }
}

pub fn max_f64(a: f64, b: f64) -> f64 {
    if a < b {
        b
    } else {
        a
    }
}

impl CounterSnapshot {
    #[allow(unused)]
    pub fn new(
        name: String,
        attributes: &[(String, String)],
        doc: String,
        value: CounterType,
    ) -> CounterSnapshot {
        let attrs: Vec<String> = attributes
            .iter()
            .map(|(k, v)| format!("{}=\"{}\"", k, v.replace('"', "\\\"")))
            .collect();
        let name = match attrs.len() {
            0 => name,
            _ => format!("{}{{{}}}", name, attrs.join(",")),
        };

        CounterSnapshot {
            name,
            doc,
            ctype: value,
        }
    }

    #[allow(unused)]
    pub fn hasdata(&self) -> bool {
        self.ctype.hasdata()
    }

    #[allow(unused)]
    pub fn serialize(&self) -> String {
        self.ctype.serialize(&self.name)
    }

    pub fn merge(&mut self, other: &CounterSnapshot) -> Result<(), ProxyErr> {
        self.ctype.merge(&other.ctype)
    }

    #[allow(unused)]
    pub fn set(&mut self, other: &CounterSnapshot) -> Result<(), ProxyErr> {
        self.ctype.set(&other.ctype)
    }

    fn delta(&mut self, other: &CounterSnapshot) -> Result<(), ProxyErr> {
        self.ctype.delta(&other.ctype)
    }

    pub(crate) fn value(&self) -> CounterValue {
        CounterValue {
            name: self.name.to_string(),
            value: self.ctype.clone(),
        }
    }

    pub(crate) fn float_value(&self) -> f64 {
        match self.ctype {
            CounterType::Counter { value } => value,
            CounterType::Gauge {
                min: _,
                max: _,
                hits,
                total,
            } => total / hits,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct JobProfile {
    pub(crate) desc: JobDesc,
    pub(crate) counters: Vec<CounterSnapshot>,
}

impl JobProfile {
    #[allow(unused)]
    pub(crate) fn merge(&mut self, other_prof: JobProfile) -> Result<(), ProxyErr> {
        self.desc.merge(other_prof.desc)?;

        /* Map all counters from self */
        let mut map: HashMap<String, CounterSnapshot> = self
            .counters
            .iter()
            .map(|v| (v.name.to_string(), v.clone()))
            .collect();

        for cnt in other_prof.counters.iter() {
            if let Some(existing) = map.get_mut(&cnt.name) {
                existing.merge(cnt)?;
            } else {
                map.insert(cnt.name.to_string(), cnt.clone());
            }
        }

        self.counters = map.values().cloned().collect();

        Ok(())
    }

    #[allow(unused)]
    pub(crate) fn reset_ranges(&mut self) -> Result<(), ProxyErr> {
        for cnt in self.counters.iter_mut() {
            match &mut cnt.ctype {
                CounterType::Gauge {
                    min,
                    max,
                    hits: _,
                    total: _,
                } => {
                    *min = f64::MAX;
                    *max = f64::MIN;
                }
                CounterType::Counter { value: _ } => {}
            }
        }

        Ok(())
    }

    #[allow(unused)]
    pub(crate) fn substract(&mut self, previous: &JobProfile) -> Result<(), ProxyErr> {
        /* Map all counters from self */
        let mut map: HashMap<String, CounterSnapshot> = self
            .counters
            .iter()
            .map(|v| (v.name.to_string(), v.clone()))
            .collect();

        for cnt in previous.counters.iter() {
            if let Some(existing) = map.get_mut(&cnt.name) {
                existing.delta(cnt)?;
            } else {
                map.insert(cnt.name.to_string(), cnt.clone());
            }
        }

        self.counters = map.values().cloned().collect();

        Ok(())
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        for c in self.counters.iter() {
            if c.name == name {
                return true;
            }
        }

        false
    }

    pub(crate) fn get(&self, name: &String) -> Option<CounterSnapshot> {
        for c in self.counters.iter() {
            if c.name == *name {
                return Some(c.clone());
            }
        }

        None
    }
}

/****************
 * IN WEBSERVER *
 ****************/

#[derive(Serialize, Deserialize)]
pub(crate) struct ApiResponse {
    pub operation: String,
    pub success: bool,
}

impl ApiResponse {
    #[allow(unused)]
    pub fn query(url: &String) -> Result<ApiResponse, Box<dyn Error>> {
        let client = reqwest::blocking::Client::new();
        let response = client.get(url).send()?;

        if response.status().is_success() {
            let resp: ApiResponse = response.json()?;
            Ok(resp)
        } else {
            Err(ProxyErr::newboxed(
                format!(
                    "Failed to query to {} got response {} : {}",
                    url,
                    response.status(),
                    response.text().unwrap_or("error".to_string())
                )
                .as_str(),
            ))
        }
    }
}
