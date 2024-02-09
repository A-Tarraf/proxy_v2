use serde::Serialize;
use std::fs::File;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use rayon::iter::{self, IntoParallelRefIterator, ParallelIterator};
use regex::Regex;
use std::io::Write;
use std::{collections::HashMap, fs, path::PathBuf, str::FromStr, time::UNIX_EPOCH};
use std::{
    collections::HashSet,
    error::Error,
    fmt::{self},
};

use meval::Expr;

use crate::proxywireprotocol::JobProfile;

struct ExtrapProjection {
    equation: String,
    expr: Expr,
    rss: f64,
}

pub(crate) struct ExtrapEval {
    path: PathBuf,
    models: HashMap<String, ExtrapProjection>,
    last_load_date: Option<u64>,
}

impl ExtrapEval {
    fn _run_extrap(path: &PathBuf) -> Result<String> {
        let out = std::process::Command::new("extrap")
            .args(["--json", &path.to_string_lossy()])
            .output()?;

        Ok(String::from_utf8(out.stdout)?)
    }

    fn _mod_time(&self) -> Result<u64> {
        // Retrieve metadata for the file
        let metadata = fs::metadata(&self.path)?;
        Ok(metadata.modified()?.duration_since(UNIX_EPOCH)?.as_secs())
    }

    fn _replace_logs(input: &str) -> Result<String> {
        /* we need to replace the logs by LN */

        let pattern = Regex::new(r"log\((.*?)\)")?;

        // Perform the replacement
        let fix = pattern.replace_all(input, |caps: &regex::Captures| {
            let x = &caps[1];
            format!("(ln({})/ln(10))", x)
        });

        let pattern = Regex::new(r"log([0-9]+)\((.*?)\)")?;

        // Perform the replacement
        let fix = pattern.replace_all(&fix, |caps: &regex::Captures| {
            let n = &caps[1];
            let x = &caps[2];
            format!("(ln({})/ln({}))", x, n)
        });

        Ok(fix.to_string())
    }

    fn _load_extrap(&mut self) -> Result<Vec<(String, Expr)>> {
        let ret = Vec::new();

        let log = ExtrapEval::_run_extrap(&self.path)?;

        /* Filter lines */
        let mut lines: Vec<String> = log
            .split('\n')
            .filter_map(|l| {
                if l.contains("Model: ")
                    || l.contains("Metric: ")
                    || l.contains("RSS: ")
                    || l.contains("Callpath")
                {
                    Some(l.to_string())
                } else {
                    None
                }
            })
            .collect();

        let lines = lines.join("\n");

        let callpath_re = Regex::new(r"Callpath: (.*)$")?;

        let mut per_callpath: Vec<(String, Vec<String>)> = Vec::new();

        for callpath in lines.split("Callpath: ") {
            /* Attempt to extract Callpath */
            let lines: Vec<&str> = callpath.split("\n").collect();

            let (callpath, idx) = if let Some(callpath) = lines.get(0) {
                if let Some(captures) = callpath_re.captures(*callpath) {
                    if let Some(call) = captures.get(1) {
                        (call.as_str(), 1)
                    } else {
                        ("", 0)
                    }
                } else {
                    ("", 0)
                }
            } else {
                ("", 0)
            };

            /* Gather metric and model on the same line */
            let metrics: Vec<String> = lines[idx..]
                .chunks(3)
                .filter_map(|v| {
                    if v.len() == 3 {
                        let merged = format!("{} ::: {} ::: {}", v[0], v[1], v[2]);
                        if merged.contains("Model")
                            && merged.contains("Metric")
                            && merged.contains("RSS")
                        {
                            Some(merged)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .collect();

            per_callpath.push((callpath.to_string(), metrics));
        }

        println!("{:?}", per_callpath);

        /* Remove previous models */
        self.models.clear();

        // Define the regular expression pattern
        let re = Regex::new(r"\s+Metric: (.*) ::: \s+Model: (.*) ::: \s+RSS: (.*)$").unwrap();

        for (callpath, metrics) in per_callpath {
            for m in metrics {
                // Perform the capture
                if let Some(captures) = re.captures(m.as_str()) {
                    // Extract captured groups
                    if let (Some(metric_value), Some(model_value), Some(rss_value)) =
                        (captures.get(1), captures.get(2), captures.get(3))
                    {
                        if let Ok(fix) = ExtrapEval::_replace_logs(model_value.as_str()) {
                            if fix != "None" {
                                match Expr::from_str(fix.to_string().as_str()) {
                                    Ok(expr) => {
                                        if let Ok(rss) = rss_value.as_str().parse::<f64>() {
                                            let name =
                                                format!("{}{}", callpath, metric_value.as_str());
                                            log::debug!(
                                                "Model for {} ({}) RSS: {}",
                                                name,
                                                fix,
                                                rss
                                            );
                                            let eval = ExtrapProjection {
                                                equation: fix.to_string(),
                                                expr,
                                                rss,
                                            };
                                            self.models.insert(name, eval);
                                        }
                                    }

                                    Err(e) => {
                                        println!("Failed to parse expression {} : {}", fix, e)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(ret)
    }

    fn check_model(&mut self) -> Result<()> {
        let cur_date = self._mod_time()?;

        if let Some(last_date) = self.last_load_date {
            /* No need to load it is already last snapshot */
            if last_date == cur_date {
                return Ok(());
            }
        }

        self._load_extrap()?;
        self.last_load_date = Some(cur_date);

        Ok(())
    }

    pub(crate) fn new(path: PathBuf) -> Result<ExtrapEval> {
        if !path.is_file() {
            return Err(anyhow!("{} is not a file", path.to_string_lossy()));
        }

        let models = HashMap::new();

        let mut ret = ExtrapEval {
            path,
            models,
            last_load_date: None,
        };

        Ok(ret)
    }

    /* From here we have the accessors we need to check the model is up to date
    each time as we do not rerun extrap if the model is not changed */

    pub(crate) fn models(&mut self) -> Result<Vec<(String, String, f64)>> {
        self.check_model()?;

        Ok(self
            .models
            .iter()
            .map(|(k, model)| (k.clone(), model.equation.clone(), model.rss))
            .collect())
    }

    pub(crate) fn evaluate(&mut self, metric: &String, value: f64) -> Result<f64> {
        self.check_model()?;
        if let Some(model) = self.models.get(metric) {
            if let Ok(func) = model.expr.clone().bind("size") {
                Ok(func(value))
            } else {
                Err(anyhow!("Failed to bind 'size' to {}", model.equation))
            }
        } else {
            Err(anyhow!("No model for metric {}", metric))
        }
    }

    pub(crate) fn plot(&mut self, metric: &String, range: &[f64]) -> Result<Vec<(f64, f64)>> {
        self.check_model()?;

        if let Some(model) = self.models.get(metric) {
            if let Ok(func) = model.expr.clone().bind("size") {
                let vals: Vec<(f64, f64)> = range.iter().map(|v| (*v, (func)(*v))).collect();
                Ok(vals)
            } else {
                Err(anyhow!("Failed to bind 'size' to {}", model.equation))
            }
        } else {
            Err(anyhow!("No model for metric {}", metric))
        }
    }
}

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
    #[allow(unused)]
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
    value: f64,
}

impl ExtrapSample {
    fn new(metric: &str, size: i32, value: f64, callpath: Option<String>) -> ExtrapSample {
        ExtrapSample {
            metric: metric.to_string(),
            callpath,
            size,
            value,
        }
    }

    fn to_jsonl_sample(&self) -> ExtrapJsonlSample {
        let mut ret = ExtrapJsonlSample {
            params: HashMap::new(),
            metric: self.metric.to_string(),
            callpath: self.callpath.clone(),
            value: self.value,
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

        /* Here we only add the final leaf in the tree
        we will need to rebuild the parent nodes afterwards
        by construction these have no child being leaves */
        for cm in common_metrics.iter() {
            for p in ret.profiles.iter() {
                let v = p.get(cm).unwrap();

                let mut metric = "various".to_string();
                let callpath;

                if cm.contains("___") {
                    /* Leaf has full callpath */
                    let callpath_vec: Vec<&str> = cm.split("___").collect();

                    if callpath_vec.len() >= 3 {
                        metric = match callpath_vec[1] {
                            "hits" | "time" | "size" => callpath_vec[1].to_string(),
                            _ => "various".to_string(),
                        };
                    }

                    callpath = Some(cm.to_string().replace("___", "->").to_string());
                } else {
                    callpath = Some(cm.to_string());
                }

                let sample = ExtrapSample::new(&metric, p.desc.size, v.float_value(), callpath);
                ret.samples.push(sample);
            }
        }

        ret
    }

    fn metric_is_transversal(&self, metric: &str) -> bool {
        for p in &self.profiles {
            if !p.contains(metric) {
                return false;
            }
        }
        true
    }

    pub(crate) fn to_jsonl(&self) -> Vec<ExtrapJsonlSample> {
        self.samples.iter().map(|v| v.to_jsonl_sample()).collect()
    }

    pub(crate) fn serialize(&self, path: &PathBuf) -> Result<(), Box<dyn Error>> {
        let mut fd = File::create(path)?;

        let samples = self.to_jsonl();

        for s in samples.iter() {
            let data = serde_json::to_vec(&s)?;
            fd.write_all(&data)?;
            fd.write_all("\n".as_bytes())?;
        }

        Ok(())
    }

    #[allow(unused)]
    pub(crate) fn sizes(&self) -> Vec<i32> {
        self.profiles.iter().map(|v| v.desc.size).collect()
    }
}
