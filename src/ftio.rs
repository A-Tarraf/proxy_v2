use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FtioArguments {
    pub freq: i32,
    pub ts: Option<String>,
    pub te: Option<String>,
    pub transformation: String,
    pub outlier: String,
    pub level: String,
    pub tol: Option<f64>,
    pub dtw: bool,
    pub no_psd: bool,
    pub autocorrelation: bool,
    pub window_adaptation: bool,
    pub frequency_hits: Option<u32>,
}

impl Default for FtioArguments {
    fn default() -> Self {
        Self {
            freq: 10,
            ts: None,
            te: None,
            transformation: "dft".to_string(),
            outlier: "Z-score".to_string(),
            level: "3".to_string(),
            tol: None,
            dtw: false,
            no_psd: false,
            autocorrelation: false,
            window_adaptation: false,
            frequency_hits: None,
        }
    }
}

impl FtioArguments {
    pub fn to_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        args.push("-f".to_string());
        args.push(self.freq.to_string());

        if let Some(ts) = &self.ts {
            args.push("-ts".to_string());
            args.push(ts.clone());
        }
        if let Some(te) = &self.te {
            args.push("-te".to_string());
            args.push(te.clone());
        }

        args.push("-tr".to_string());
        args.push(self.transformation.clone());

        args.push("-o".to_string());
        args.push(self.outlier.clone());

        args.push("-le".to_string());
        args.push(self.level.clone());

        if let Some(tol) = self.tol {
            args.push("-t".to_string());
            args.push(tol.to_string());
        }

        if self.dtw {
            args.push("-d".to_string());
        }
        if self.no_psd {
            args.push("-np".to_string());
        }
        if self.autocorrelation {
            args.push("-c".to_string());
        }
        if self.window_adaptation {
            args.push("-w".to_string());
        }
        if let Some(fh) = self.frequency_hits {
            args.push("-fh".to_string());
            args.push(fh.to_string());
        }

        args
    }

    pub fn from_json(json_str: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json_str)
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

pub struct FtioClient {
    context: Arc<zmq::Context>,
    address: String,
    arguments: RwLock<FtioArguments>,
}

impl FtioClient {
    pub fn new(address: &str) -> Self {
        println!("FTIO client created");
        Self {
            context: Arc::new(zmq::Context::new()),
            address: address.to_string(),
            arguments: RwLock::new(FtioArguments::default()),
        }
    }

    pub fn get_arguments(&self) -> std::sync::RwLockReadGuard<'_, FtioArguments> {
        self.arguments.read().unwrap()
    }

    pub fn set_arguments(&self, new_args: FtioArguments) {
        let mut args = self.arguments.write().unwrap();
        *args = new_args;
    }
}
