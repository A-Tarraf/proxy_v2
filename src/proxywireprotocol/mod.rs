use crate::proxy_common::unix_ts;
use crate::proxy_common::ProxyErr;

use serde::{Deserialize, Serialize};
use std::{collections::HashMap, env, error::Error};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[repr(u8)]
pub(crate) enum ProxyCommandType {
    REGISTER = 0,
    SET = 1,
    GET = 2,
    LIST = 3,
}

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

impl CounterType {
    pub fn newcounter() -> CounterType {
        CounterType::Counter { value: 0.0 }
    }

    pub fn newgauge() -> CounterType {
        CounterType::Gauge {
            min: 0.0,
            max: 0.0,
            hits: 0.0,
            total: 0.0,
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

fn min_f64(a: &f64, b: &f64) -> f64 {
    if *a < *b {
        *a
    } else {
        *b
    }
}

fn max_f64(a: &f64, b: &f64) -> f64 {
    if *a < *b {
        *b
    } else {
        *a
    }
}

impl CounterSnapshot {
    pub fn serialize(&self) -> String {
        match self.ctype {
            CounterType::Counter { value } => {
                format!("{} {}\n", self.name, value)
            }
            CounterType::Gauge {
                min,
                max,
                hits,
                total,
            } => {
                format!(
                    "avg_{} {}\nmin_{} {}\nmax_{} {}\n",
                    self.name,
                    total / hits,
                    self.name,
                    min,
                    self.name,
                    max
                )
            }
        }
    }

    pub fn check_same_type(&self, other: &CounterSnapshot) -> Result<(), ProxyErr> {
        match (&self.ctype, &other.ctype) {
            (CounterType::Gauge { .. }, CounterType::Gauge { .. }) => Ok(()),
            (CounterType::Counter { .. }, CounterType::Counter { .. }) => Ok(()),
            _ => Err(ProxyErr::new("Both instances are not of the same variant")),
        }
    }

    pub fn merge(&mut self, other: &CounterSnapshot) -> Result<(), ProxyErr> {
        self.check_same_type(other)?;

        match other.ctype {
            CounterType::Counter { value } => {
                /* For a counter we simply add the local and remote values */
                self.ctype = match &self.ctype {
                    CounterType::Counter { value: svalue } => CounterType::Counter {
                        value: value + svalue,
                    },
                    _ => unreachable!(),
                };
            }
            CounterType::Gauge {
                min,
                max,
                hits,
                total,
            } => {
                /* Here we sum the values and keep min and max accordingly */
                self.ctype = match self.ctype {
                    CounterType::Gauge {
                        min: smin,
                        max: smax,
                        hits: shits,
                        total: stotal,
                    } => CounterType::Gauge {
                        min: min_f64(&smin, &min),
                        max: max_f64(&smax, &max),
                        hits: hits + shits,
                        total: total + stotal,
                    },
                    _ => unreachable!(),
                };
            }
        }

        Ok(())
    }

    fn delta(&mut self, other: &CounterSnapshot) -> Result<(), ProxyErr> {
        self.check_same_type(other)?;

        match other.ctype {
            CounterType::Counter { value } => {
                /* For a counter we simply add the local and remote values */
                self.ctype = match &self.ctype {
                    CounterType::Counter { value: svalue } => CounterType::Counter {
                        value: svalue - value,
                    },
                    _ => unreachable!(),
                };
            }
            CounterType::Gauge {
                min,
                max,
                hits,
                total,
            } => {
                /* Here we sum the values and keep min and max accordingly */
                self.ctype = match self.ctype {
                    CounterType::Gauge {
                        min: smin,
                        max: smax,
                        hits: shits,
                        total: stotal,
                    } => CounterType::Gauge {
                        min: min_f64(&smin, &min),
                        max: max_f64(&smax, &max),
                        hits: shits - hits,
                        total: stotal - total,
                    },
                    _ => unreachable!(),
                };
            }
        }

        Ok(())
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
