use clap::builder::Str;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

use crate::trace::TraceExport;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FtioArguments {
    pub freq: f64,
    pub memory_limit: Option<f64>,
    pub ts: Option<f64>,
    pub te: Option<f64>,
    pub transformation: String,
    pub level: Option<i32>,
    pub wavelet: Option<String>,
    pub outlier: String,
    pub periodicity_detection: Option<String>,
    pub tol: Option<f64>,
    pub dtw: bool,
    pub no_psd: bool,
    pub n_freq: i32,
    pub fourier_fit: bool,
    pub autocorrelation: bool,
    pub window_adaptation: Option<String>,
    pub hits: Option<f64>,
    pub filter_type: Option<String>,
    pub filter_cutoff: Option<f64>,
    pub filter_order: Option<i32>,
}

impl Default for FtioArguments {
    fn default() -> Self {
        Self {
            freq: 10.0,
            memory_limit: None,
            ts: None,
            te: None,
            transformation: "dft".to_string(),
            level: None,
            wavelet: None,
            outlier: "z-score".to_string(),
            periodicity_detection: None,
            tol: None,
            dtw: false,
            no_psd: true,
            n_freq: 10,
            fourier_fit: false,
            autocorrelation: false,
            window_adaptation: None,
            hits: None,
            filter_type: None,
            filter_cutoff: None,
            filter_order: None,
        }
    }
}

impl FtioArguments {
    pub fn to_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        args.push("--freq".to_string());
        args.push(self.freq.to_string());

        if let Some(memory_limit) = self.memory_limit {
            args.push("--memory-limit".to_string());
            args.push(memory_limit.to_string());
        }

        if let Some(ts) = self.ts {
            args.push("--ts".to_string());
            args.push(ts.to_string());
        }

        if let Some(te) = self.te {
            args.push("--te".to_string());
            args.push(te.to_string());
        }

        args.push("--transformation".to_string());
        args.push(self.transformation.clone());

        if let Some(level) = self.level {
            args.push("--level".to_string());
            args.push(level.to_string());
        }

        if let Some(wavelet) = &self.wavelet {
            args.push("--wavelet".to_string());
            args.push(wavelet.clone());
        }

        args.push("--outlier".to_string());
        args.push(self.outlier.clone());

        if let Some(p) = &self.periodicity_detection {
            args.push("--periodicity-detection".to_string());
            args.push(p.clone());
        }

        if let Some(tol) = self.tol {
            args.push("--tol".to_string());
            args.push(tol.to_string());
        }

        if self.dtw {
            args.push("--dtw".to_string());
        }

        if self.no_psd {
            args.push("--no-psd".to_string());
        }

        args.push("--n_freq".to_string());
        args.push(self.n_freq.to_string());

        if self.fourier_fit {
            args.push("--fourier-fit".to_string());
        }

        if self.autocorrelation {
            args.push("--autocorrelation".to_string());
        }

        if let Some(win) = &self.window_adaptation {
            args.push("--window-adaptation".to_string());
            args.push(win.clone());
        }

        if let Some(h) = self.hits {
            args.push("--hits".to_string());
            args.push(h.to_string());
        }

        if let Some(ft) = &self.filter_type {
            args.push("--filter-type".to_string());
            args.push(ft.clone());
        }

        if let Some(fc) = self.filter_cutoff {
            args.push("--filter-cutoff".to_string());
            args.push(fc.to_string());
        }

        if let Some(fo) = self.filter_order {
            args.push("--filter-order".to_string());
            args.push(fo.to_string());
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

    pub fn send_receive(&self, export: TraceExport) -> Result<String, Box<dyn std::error::Error>> {
        let socket = self.context.socket(zmq::REQ)?;
        socket.set_rcvtimeo(3000)?;
        socket.set_sndtimeo(3000)?;
        socket.connect(&self.address)?;

        let args = self.get_arguments();
        let payload = serde_json::json!({
            "argv": args.to_args(),
            "metrics": export,
            "disable_parallel": false
        });
        let payload_str = serde_json::to_string(&payload)?;

        socket.send(&payload_str, 0)?;

        let reply = socket
            .recv_string(0)?
            .map_err(|e| format!("Recv error: {:?}", e))?;

        let json_start = reply
            .find("[")
            .ok_or("JSON array not found in FTIO output")?;
        let json_part = reply[json_start..].to_string();

        Ok(json_part)
    }

    pub fn ping_server(&self) -> bool {
        let socket = match self.context.socket(zmq::REQ) {
            Ok(s) => s,
            Err(e) => {
                log::error!("Failed to create REQ socket: {}", e);
                return false;
            }
        };

        socket.set_rcvtimeo(500).unwrap();
        socket.set_sndtimeo(500).unwrap();
        socket.connect(&self.address).unwrap();

        if socket.send("ping", 0).is_err() {
            return false;
        }

        match socket.recv_string(0) {
            Ok(Ok(reply)) if reply == "pong" => true,
            _ => false,
        }
    }
}
