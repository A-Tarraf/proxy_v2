use anyhow::anyhow;
use clap::builder::Str;
use md5::Digest;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use serde::de::value;

use super::proxywireprotocol::{JobDesc, JobProfile};
use crate::extrap::ExtrapModel;
use crate::proxy_common::{check_prefix_dir, list_files_with_ext_in, ProxyErr};
use std::collections::HashMap;
use std::error::Error;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::{any, fs};

use anyhow::Result;

use crate::extrap::ExtrapEval;

pub(crate) struct ProfileView {
    profdir: PathBuf,
    profiles: RwLock<HashMap<String, JobProfile>>,
    models: Mutex<HashMap<String, ExtrapEval>>,
}

impl ProfileView {
    pub(crate) fn _get_profile(path: &String) -> Result<JobProfile, Box<dyn Error>> {
        let file = fs::File::open(path)?;
        let content: JobProfile = serde_json::from_reader(file)?;
        Ok(content)
    }

    fn extrap_filename(&self, command: &str) -> (Option<PathBuf>, String) {
        let digest = md5::compute(command);
        let mut path = self.profdir.clone();
        let hash = format!("{:x}", digest);
        path.push(format!("{}.jsonl", hash));

        if path.is_file() {
            return (Some(path.to_path_buf()), hash);
        }

        (None, hash)
    }

    pub(crate) fn get_profile(&self, jobid: &str) -> Result<JobProfile, Box<dyn Error>> {
        if let Some(prof) = self.profiles.read().unwrap().get(jobid) {
            let mut ret = prof.clone();
            if ret.add_duration()? {
                self.generate_extrap_model(&ret.desc)?;
            }
            return Ok(ret);
        }

        let mut path = self.profdir.clone();
        path.push(format!("{}.profile", jobid));
        let mut ret = ProfileView::_get_profile(&path.to_string_lossy().to_string())?;
        if ret.add_duration()? {
            self.generate_extrap_model(&ret.desc)?;
        }
        Ok(ret)
    }

    pub(crate) fn get_jsonl_by_cmd(&self, command: &str) -> Result<String, Box<dyn Error>> {
        if let (Some(path), _) = self.extrap_filename(&command) {
            let mut fd = fs::File::open(path)?;
            let mut data: Vec<u8> = Vec::new();
            fd.read_to_end(&mut data)?;

            let data = String::from_utf8(data)?;
            Ok(data)
        } else {
            Err(ProxyErr::newboxed("No model for this profile"))
        }
    }

    pub(crate) fn get_jsonl(&self, desc: &JobDesc) -> Result<String, Box<dyn Error>> {
        let mut data = self.get_jsonl_by_cmd(&desc.command)?;

        if !data.contains("walltime") {
            /* This is a previous JSONL lets regenerate */
            self.generate_extrap_model(&desc)?;
            data = self.get_jsonl_by_cmd(&desc.command)?;
        }

        Ok(data)
    }

    pub(crate) fn refresh_profiles(&self) -> Result<(), Box<dyn Error>> {
        /* Load profiles and existing extra-p models */

        let ret = list_files_with_ext_in(&self.profdir, "profile")?;
        let mut ht = self.profiles.write().unwrap();
        let mut model_ht = self.models.lock().unwrap();

        for p in ret.iter() {
            if !ht.contains_key(p) {
                let content = Self::_get_profile(p)?;
                let extrap_model = self.extrap_filename(&content.desc.command);

                ht.insert(content.desc.jobid.clone(), content);

                if let (Some(extrap_model), hash) = extrap_model {
                    if extrap_model.is_file() && !model_ht.contains_key(&hash) {
                        model_ht.insert(hash, ExtrapEval::new(extrap_model)?);
                    }
                }
            }
        }

        Ok(())
    }

    pub(crate) fn gather_by_command(&self) -> HashMap<String, Vec<JobDesc>> {
        let mut ret: HashMap<String, Vec<JobDesc>> = HashMap::new();

        let ht = self.profiles.read().unwrap();

        for prof in ht.values() {
            let cmd_vec = ret.entry(prof.desc.command.clone()).or_default();
            cmd_vec.push(prof.desc.clone());
        }

        ret
    }

    pub(crate) fn filter_by_command(&self, cmd: &String) -> Vec<JobDesc> {
        self.profiles
            .read()
            .unwrap()
            .par_iter()
            .filter_map(|(_, p)| {
                if p.desc.command == *cmd {
                    Some(p.desc.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    #[allow(unused)]
    pub(crate) fn get_profile_list(&self) -> Vec<JobDesc> {
        self.profiles
            .read()
            .unwrap()
            .values()
            .map(|prof| prof.desc.clone())
            .collect()
    }

    pub(crate) fn extrap_model_list(&self, desc: &JobDesc) -> Result<Vec<(String, String, f64)>> {
        let cmd_hash = md5::compute(&desc.command);
        let hash = format!("{:x}", cmd_hash);

        if let Some(m) = self.models.lock().unwrap().get_mut(&hash) {
            Ok(m.models()?)
        } else {
            Err(anyhow!("Failed to retrieve an extra-p model for {}", hash))
        }
    }

    pub(crate) fn extrap_model_eval(
        &self,
        desc: &JobDesc,
        metric: String,
        size: f64,
    ) -> Result<(f64, f64)> {
        let cmd_hash = md5::compute(&desc.command);
        let hash = format!("{:x}", cmd_hash);

        if let Some(m) = self.models.lock().unwrap().get_mut(&hash) {
            let val = m.evaluate(&metric, size)?;
            Ok((size, val))
        } else {
            Err(anyhow!("Failed to retrieve an extra-p model for {}", hash))
        }
    }

    pub(crate) fn extrap_model_plot(
        &self,
        desc: &JobDesc,
        metric: String,
        points: &[f64],
    ) -> Result<Vec<(f64, f64)>> {
        let cmd_hash = md5::compute(&desc.command);
        let hash = format!("{:x}", cmd_hash);

        /* Try to load models too */
        if self.models.lock().unwrap().get(&hash).is_none() {
            let _ = self.refresh_profiles();
        }

        if let Some(m) = self.models.lock().unwrap().get_mut(&hash) {
            let vals = m.plot(&metric, points)?;
            Ok(vals)
        } else {
            Err(anyhow!("Failed to retrieve an extra-p model for {}", hash))
        }
    }

    pub(crate) fn generate_profile_points(
        &self,
        desc: &JobDesc,
    ) -> Result<HashMap<String, Vec<(i32, f64)>>> {
        let matching_desc = self.filter_by_command(&desc.command);

        if !matching_desc.is_empty() {
            let mut profiles: Vec<JobProfile> = matching_desc
                .par_iter()
                .filter_map(|v| self.get_profile(&v.jobid).ok())
                .collect();

            profiles.sort_by(|a, b| a.desc.size.cmp(&b.desc.size));

            let mut ret: HashMap<String, Vec<(i32, f64)>> = HashMap::new();

            for p in profiles {
                let size: i32 = p.desc.size;
                for m in p.counters {
                    let val_vec = ret.entry(m.name).or_default();
                    val_vec.push((size, m.ctype.value()));
                }
            }

            return Ok(ret);
        }

        Err(anyhow!(
            "Failed to find command {} in previous profiles",
            desc.command
        ))
    }

    pub(crate) fn generate_extrap_model_for_profiles(
        &self,
        profiles: Vec<JobProfile>,
        hash: Digest,
    ) -> Result<(), Box<dyn Error>> {
        let model = ExtrapModel::new(profiles);

        let mut target_dir = self.profdir.clone();
        let hash: String = format!("{:x}", hash);
        let fname: String = format!("{}.jsonl", hash);
        target_dir.push(fname);

        /* Save the new model */
        model.serialize(&target_dir)?;

        /* Make sure the model is in the Evaluation list */
        if let Ok(model_ht) = self.models.lock().as_mut() {
            model_ht.entry(hash).or_insert(ExtrapEval::new(target_dir)?);
            /* Otherwise nothing to do as we use the metadata when pulling from the model */
        }

        Ok(())
    }

    pub(crate) fn generate_extrap_model(&self, desc: &JobDesc) -> Result<(), Box<dyn Error>> {
        let gather_by_cmd = self.gather_by_command();

        if let Some(myjob) = gather_by_cmd.get(&desc.command) {
            let profiles: Vec<JobProfile> = myjob
                .iter()
                .filter_map(|v| self.get_profile(&v.jobid).ok())
                .collect();

            let cmd_hash = md5::compute(&desc.command);
            self.generate_extrap_model_for_profiles(profiles, cmd_hash)?;
        }

        Ok(())
    }

    pub(crate) fn saveprofile(
        &self,
        mut snap: JobProfile,
        desc: &JobDesc,
    ) -> Result<(), Box<dyn Error>> {
        let mut target_dir = self.profdir.clone();

        let fname = format!("{}.profile", desc.jobid);

        target_dir.push(fname);

        log::debug!(
            "Saving profile for {} in {}",
            desc.jobid,
            target_dir.to_str().unwrap_or("")
        );

        // Nan / Infinite values seralize to null and cannot be parsed back
        // This is why we make this pass on all values to ensure we do not store
        // invalid data
        snap.counters.iter_mut().for_each(|c| c.clean());

        let file = fs::File::create(target_dir)?;

        serde_json::to_writer(file, &snap)?;

        self.profiles
            .write()
            .unwrap()
            .insert(desc.jobid.clone(), snap);

        self.generate_extrap_model(desc)?;

        Ok(())
    }

    pub(crate) fn new(profdir: &PathBuf) -> Result<ProfileView, Box<dyn Error>> {
        let profdir = check_prefix_dir(profdir, "profiles")?;

        let ret = ProfileView {
            profdir,
            profiles: RwLock::new(HashMap::new()),
            models: Mutex::new(HashMap::new()),
        };

        ret.refresh_profiles()?;

        Ok(ret)
    }
}
