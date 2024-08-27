use crate::proxy_common::ProxyErr;
use serde::Serialize;
use std::collections::HashMap;
use std::error::Error;
use std::process::Command;

pub type SqueueJobInfo = HashMap<String, String>;

#[derive(Serialize)]
pub struct SqueueJobList {
    pub jobs: HashMap<String, SqueueJobInfo>,
}

impl SqueueJobList {
    pub fn init() -> Result<SqueueJobList, Box<dyn Error>> {
        let mut ret = SqueueJobList {
            jobs: HashMap::new(),
        };

        ret.load()?;

        Ok(ret)
    }

    pub fn job_cmd(&self, id: &str) -> Option<String> {
        if let Some(j) = self.jobs.get(id) {
            if let Some(c) = j.get("COMMENT") {
                return Some(c.to_string());
            }
        }

        None
    }

    pub fn load(&mut self) -> Result<(), Box<dyn Error>> {
        let output = Command::new("squeue")
            .args(["--format", "%all"])
            .output()?
            .stdout;
        let output = String::from_utf8(output)?;

        let lines: Vec<&str> = output.split("\n").collect();

        /* First line contains the headers */
        let header: Vec<&str> = if let Some(sp) = lines.first().map(|d| d.split('|')) {
            sp.collect()
        } else {
            vec![]
        };

        if header.is_empty() {
            return Err(ProxyErr::newboxed(
                "Failed to retrieve squeue information".to_string(),
            ));
        }

        /* Other lines are the entries */
        for i in 1..lines.len() {
            if let Some(l) = lines.get(i) {
                let entry: Vec<&str> = l.split('|').collect();

                if l.is_empty() {
                    continue;
                }

                if entry.len() != header.len() {
                    println!(
                        "Error parsing squeue output {:?} does not match header len",
                        entry
                    );
                }

                let mut infos: HashMap<String, String> = HashMap::new();

                for j in 0..header.len() {
                    infos.insert(
                        header.get(j).unwrap().to_string(),
                        entry.get(j).unwrap().to_string(),
                    );
                }

                if let Some(jobid) = infos.get("JOBID") {
                    self.jobs.insert(jobid.to_string(), infos);
                }
            }
        }

        Ok(())
    }
}
