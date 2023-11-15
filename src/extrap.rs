use serde::Serialize;
use std::fs::File;

use std::io::Write;
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt::{self},
    path::PathBuf,
};

use crate::proxywireprotocol::{CounterSnapshot, JobProfile};

/// This represents a line in the JSONL
/// output of ExtraP json format is:
/// ```json
/// {"params":{"x":1,"y":1},"metric":"metr","callpath":"test","value":2}
/// ```
/// On each line of the file.

#[derive(Serialize)]
pub(crate) struct ExtrapJsonlSample {
    params: HashMap<String, f64>,
    metric: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    callpath: Option<String>,
    value: f64,
}

impl fmt::Display for ExtrapJsonlSample {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let params: Vec<String> = self
            .params
            .iter()
            .map(|v| format!("{} = {}", v.0, v.1))
            .collect();
        let params = params.join(" ");
        let callpath = if let Some(cp) = &self.callpath {
            format!(" @ {}", cp)
        } else {
            "".to_string()
        };
        write!(
            f,
            "({}){} = {}{}",
            params, self.metric, self.value, callpath
        )
    }
}

impl ExtrapJsonlSample {
    fn new(metric: String, callpath: Option<String>, value: f64) -> ExtrapJsonlSample {
        ExtrapJsonlSample {
            params: HashMap::new(),
            metric,
            callpath,
            value,
        }
    }

    fn push_param(&mut self, param: &str, value: f64) -> &mut ExtrapJsonlSample {
        self.params.insert(param.to_string(), value);
        self
    }
}

struct ExtrapSample {
    metric: String,
    callpath: Option<String>,
    size: i32,
    value: CounterSnapshot,
}

impl ExtrapSample {
    fn new(metric: &str, size: i32, value: CounterSnapshot) -> ExtrapSample {
        let mut spl = metric.split('{');
        let name_without_params = spl.next().unwrap().to_string();

        let group_name = if let Some(options) = spl.next() {
            format!("{{{}", options)
        } else {
            name_without_params.to_string()
        };

        let metric = if name_without_params.contains("time_") {
            "time"
        } else if name_without_params.contains("hits_") {
            "hits"
        } else if name_without_params.contains("size_") {
            "size"
        } else {
            name_without_params.as_str()
        };

        ExtrapSample {
            metric: metric.to_string(),
            callpath: Some(group_name),
            size,
            value,
        }
    }

    fn to_jsonl_sample(&self) -> ExtrapJsonlSample {
        let mut ret = ExtrapJsonlSample {
            params: HashMap::new(),
            metric: self.metric.to_string(),
            callpath: self.callpath.clone(),
            value: self.value.float_value(),
        };

        ret.push_param("size", self.size as f64);

        ret
    }
}

pub(crate) struct ExtrapModel {
    profiles: Vec<JobProfile>,
    samples: Vec<ExtrapSample>,
}

impl ExtrapModel {
    fn _get_transversal_metrics(&self) -> HashSet<String> {
        // First get a list of transversal metrics
        let mut metrics: HashSet<String> = HashSet::new();

        for p in self.profiles.iter() {
            for v in p.counters.iter() {
                metrics.insert(v.name.to_string());
            }
        }

        let metrics: HashSet<String> = metrics
            .iter()
            .filter(|v| self.metric_is_transversal(v))
            .cloned()
            .collect();
        metrics
    }

    pub(crate) fn new(profiles: Vec<JobProfile>) -> ExtrapModel {
        let mut ret = ExtrapModel {
            profiles,
            samples: Vec::new(),
        };

        /* Make sure to sort profiles by size */
        ret.profiles.sort_by(|a, b| a.desc.size.cmp(&b.desc.size));

        let common_metrics = ret._get_transversal_metrics();

        for cm in common_metrics.iter() {
            for p in ret.profiles.iter() {
                let v = p.get(cm).unwrap();
                let sample = ExtrapSample::new(cm, p.desc.size, v);
                ret.samples.push(sample);
            }
        }

        ret
    }

    fn metric_is_transversal(&self, metric: &String) -> bool {
        for p in &self.profiles {
            if !p.contains(metric.as_str()) {
                return false;
            }
        }
        true
    }

    pub(crate) fn to_jsonl(&self) -> Vec<ExtrapJsonlSample> {
        self.samples.iter().map(|v| v.to_jsonl_sample()).collect()
    }

    pub(crate) fn serialize(&self, path: PathBuf) -> Result<(), Box<dyn Error>> {
        let mut fd = File::create(path)?;

        let samples = self.to_jsonl();

        for s in samples.iter() {
            let data = serde_json::to_vec(&s)?;
            fd.write_all(&data)?;
            fd.write_all("\n".as_bytes())?;
        }

        Ok(())
    }

    pub(crate) fn sizes(&self) -> Vec<i32> {
        self.profiles.iter().map(|v| v.desc.size).collect()
    }
}
