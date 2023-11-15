use super::proxywireprotocol::{JobDesc, JobProfile};
use crate::extrap::ExtrapModel;
use crate::proxy_common::{check_prefix_dir, list_files_with_ext_in, ProxyErr};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::io::Read;
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

    fn extrap_filename(&self, desc: &JobDesc) -> Option<PathBuf> {
        let digest = md5::compute(&desc.command);
        let mut path = self.profdir.clone();
        path.push(format!("{:x}.jsonl", digest));

        if path.is_file() {
            return Some(path.to_path_buf());
        }

        None
    }

    pub(crate) fn get_profile(&self, jobid: &String) -> Result<JobProfile, Box<dyn Error>> {
        let mut path = self.profdir.clone();
        path.push(format!("{}.profile", jobid));
        ProfileView::_get_profile(&path.to_string_lossy().to_string())
    }

    pub(crate) fn get_jsonl(&self, desc: &JobDesc) -> Result<String, Box<dyn Error>> {
        if let Some(path) = self.extrap_filename(desc) {
            let mut fd = fs::File::open(path)?;
            let mut data: Vec<u8> = Vec::new();
            fd.read_to_end(&mut data)?;

            let data = String::from_utf8(data)?;
            return Ok(data);
        } else {
            return Err(ProxyErr::newboxed("No model for this profile"));
        }
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
            let fname = format!("{:x}.jsonl", cmd_hash);
            target_dir.push(fname);
            model.serialize(target_dir)?;
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

        Ok(ProfileView {
            profdir,
            profiles: RwLock::new(HashMap::new()),
        })
    }
}
