use super::proxywireprotocol::{JobDesc, JobProfile};
use crate::proxy_common::{check_prefix_dir, list_files_with_ext_in};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;

pub(crate) struct ProfileView {
    profdir: PathBuf,
    profiles: RwLock<HashMap<String, (String, JobDesc)>>,
}

impl ProfileView {
    pub(crate) fn _get_profile(path: &String) -> Result<JobProfile, Box<dyn Error>> {
        let file = fs::File::open(path)?;
        let content: JobProfile = serde_json::from_reader(file)?;
        Ok(content)
    }

    pub(crate) fn get_profile(&self, jobid: &String) -> Result<JobProfile, Box<dyn Error>> {
        let mut path = self.profdir.clone();
        path.push(format!("{}.profile", jobid));
        ProfileView::_get_profile(&path.to_string_lossy().to_string())
    }

    pub(crate) fn refresh_profiles(&self) -> Result<(), Box<dyn Error>> {
        let ret = list_files_with_ext_in(&self.profdir, "profile")?;
        let mut ht = self.profiles.write().unwrap();

        for p in ret.iter() {
            if !ht.contains_key(p) {
                let content = Self::_get_profile(p)?;
                ht.insert(content.desc.jobid.clone(), (p.to_string(), content.desc));
            }
        }

        Ok(())
    }

    pub(crate) fn gather_by_command(&self) -> HashMap<String, Vec<JobDesc>> {
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

    pub(crate) fn get_profile_list(&self) -> Vec<JobDesc> {
        self.profiles
            .read()
            .unwrap()
            .values()
            .map(|(_, v)| v.clone())
            .collect()
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

        Ok(())
    }

    pub(crate) fn new(profdir: &PathBuf) -> Result<ProfileView, Box<dyn Error>> {
        let profdir = check_prefix_dir(profdir, "profiles")?;

        Ok(ProfileView {
            profdir,
            profiles: RwLock::new(HashMap::new()),
        })
    }
}
