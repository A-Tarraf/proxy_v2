use anyhow::anyhow;
use clap::builder::Str;
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
    profiles: RwLock<HashMap<String, (String, JobDesc)>>,
    models: Mutex<HashMap<String, ExtrapEval>>,
}

impl ProfileView {
    pub(crate) fn _get_profile(path: &String) -> Result<JobProfile, Box<dyn Error>> {
        let file = fs::File::open(path)?;
        let content: JobProfile = serde_json::from_reader(file)?;
        Ok(content)
    }

    fn extrap_filename(&self, desc: &JobDesc) -> (Option<PathBuf>, String) {
        let digest = md5::compute(&desc.command);
        let mut path = self.profdir.clone();
        let hash = format!("{:x}", digest);
        path.push(format!("{}.jsonl", hash));

        if path.is_file() {
            return (Some(path.to_path_buf()), hash);
        }

        (None, hash)
    }

    pub(crate) fn get_profile(&self, jobid: &String) -> Result<JobProfile, Box<dyn Error>> {
        let mut path = self.profdir.clone();
        path.push(format!("{}.profile", jobid));
        ProfileView::_get_profile(&path.to_string_lossy().to_string())
    }

    pub(crate) fn get_jsonl(&self, desc: &JobDesc) -> Result<String, Box<dyn Error>> {
        if let (Some(path), _) = self.extrap_filename(desc) {
            let mut fd = fs::File::open(path)?;
            let mut data: Vec<u8> = Vec::new();
            fd.read_to_end(&mut data)?;

            let data = String::from_utf8(data)?;
            Ok(data)
        } else {
            Err(ProxyErr::newboxed("No model for this profile"))
        }
    }

    pub(crate) fn refresh_profiles(&self) -> Result<(), Box<dyn Error>> {
        /* Load profiles and existing extra-p models */

        let ret = list_files_with_ext_in(&self.profdir, "profile")?;
        let mut ht = self.profiles.write().unwrap();
        let mut model_ht = self.models.lock().unwrap();

        for p in ret.iter() {
            if !ht.contains_key(p) {
                let content = Self::_get_profile(p)?;
                let extrap_model = self.extrap_filename(&content.desc);

                ht.insert(content.desc.jobid.clone(), (p.to_string(), content.desc));

                if let (Some(extrap_model), hash) = extrap_model {
                    if extrap_model.is_file() {
                        model_ht.insert(hash, ExtrapEval::new(extrap_model)?);
                    }
                }
            }
        }

        Ok(())
    }

    pub(crate) fn gather_by_command(&self) -> HashMap<String, Vec<JobDesc>> {
        self.refresh_profiles().unwrap_or_default();

        let mut ret: HashMap<String, Vec<JobDesc>> = HashMap::new();

        let ht = self.profiles.read().unwrap();

        for (_, v) in ht.values() {
            if !ret.contains_key(&v.command) {
                ret.insert(v.command.to_string(), Vec::new());
            }
            let vec = ret.get_mut(&v.command).unwrap();
            vec.push(v.clone());
        }

        ret
    }

    #[allow(unused)]
    pub(crate) fn get_profile_list(&self) -> Vec<JobDesc> {
        self.refresh_profiles().unwrap_or_default();
        self.profiles
            .read()
            .unwrap()
            .values()
            .map(|(_, v)| v.clone())
            .collect()
    }

    fn extrap_model_list(&self, desc: &JobDesc) -> Result<Vec<(String, String, f64)>> {
        let cmd_hash = md5::compute(&desc.command);
        let hash = format!("{:x}", cmd_hash);

        if let Some(m) = self.models.lock().unwrap().get_mut(&hash) {
            Ok(m.models()?)
        } else {
            Err(anyhow!("Failed to retrieve an extra-p model for {}", hash))
        }
    }

    fn extrap_model_eval(&self, desc: &JobDesc, metric: String, size: f64) -> Result<(f64, f64)> {
        let cmd_hash = md5::compute(&desc.command);
        let hash = format!("{:x}", cmd_hash);

        if let Some(m) = self.models.lock().unwrap().get_mut(&hash) {
            let val = m.evaluate(&metric, size)?;
            Ok((size, val))
        } else {
            Err(anyhow!("Failed to retrieve an extra-p model for {}", hash))
        }
    }

    fn extrap_model_plot(
        &self,
        desc: &JobDesc,
        metric: String,
        points: &[f64],
    ) -> Result<Vec<(f64, f64)>> {
        let cmd_hash = md5::compute(&desc.command);
        let hash = format!("{:x}", cmd_hash);

        if let Some(m) = self.models.lock().unwrap().get_mut(&hash) {
            let vals = m.plot(&metric, points)?;
            Ok(vals)
        } else {
            Err(anyhow!("Failed to retrieve an extra-p model for {}", hash))
        }
    }

    fn generate_extrap_model(&self, desc: &JobDesc) -> Result<(), Box<dyn Error>> {
        self.refresh_profiles()?;
        let gather_by_cmd = self.gather_by_command();

        if let Some(myjob) = gather_by_cmd.get(&desc.command) {
            let profiles: Vec<JobProfile> = myjob
                .iter()
                .filter_map(|v| self.get_profile(&v.jobid).ok())
                .collect();

            let model = ExtrapModel::new(profiles);

            let cmd_hash = md5::compute(&desc.command);

            let mut target_dir = self.profdir.clone();
            let hash: String = format!("{:x}", cmd_hash);
            let fname: String = format!("{}.jsonl", hash);
            target_dir.push(fname);

            /* Save the new model */
            model.serialize(&target_dir)?;

            /* Make sure the model is in the Evaluation list */
            if let Ok(model_ht) = self.models.lock().as_mut() {
                if !model_ht.contains_key(&hash) {
                    let model = ExtrapEval::new(target_dir)?;
                    model_ht.insert(hash, model);
                }
                /* Otherwise nothing to do as we use the metadata when pulling from the model */
            }
        }

        Ok(())
    }

    pub(crate) fn saveprofile(
        &self,
        snap: JobProfile,
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

        let file = fs::File::create(target_dir)?;

        serde_json::to_writer(file, &snap)?;

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
