use clap::builder::Str;
use rmp_serde::{decode, encode};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

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
    pub tol: f64,
    pub dtw: bool,
    pub no_psd: bool,
    pub n_freq: i32,
    pub fourier_fit: bool,
    pub autocorrelation: bool,
    pub window_adaptation: Option<String>,
    pub hits: Option<f64>,
    pub filter_type: Option<String>,
    pub filter_cutoff: Option<f64>,
    pub filter_cutoff2: Option<f64>,
    pub filter_order: Option<i32>,
    pub custom_args: Option<String>,
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
            tol: 0.8,
            dtw: false,
            no_psd: true,
            n_freq: 10,
            fourier_fit: false,
            autocorrelation: false,
            window_adaptation: None,
            hits: None,
            filter_type: None,
            filter_cutoff: None,
            filter_cutoff2: None,
            filter_order: None,
            custom_args: None,
        }
    }
}

impl FtioArguments {
    pub fn to_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        args.push("--freq".to_string());
        args.push(self.freq.to_string());

        if let Some(memory_limit) = self.memory_limit {
            args.push("--memory_limit".to_string());
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
            args.push("--periodicity_detection".to_string());
            args.push(p.clone());
        }

        args.push("--tol".to_string());
        args.push(self.tol.to_string());

        if self.dtw {
            args.push("--dtw".to_string());
        }

        if self.no_psd {
            args.push("--no-psd".to_string());
        }

        args.push("--n_freq".to_string());
        args.push(self.n_freq.to_string());

        if self.fourier_fit {
            args.push("--fourier_fit".to_string());
        }

        if self.autocorrelation {
            args.push("--autocorrelation".to_string());
        }

        if let Some(win) = &self.window_adaptation {
            args.push("--window_adaptation".to_string());
            args.push(win.clone());
        }

        if let Some(h) = self.hits {
            args.push("--hits".to_string());
            args.push(h.to_string());
        }

        if let Some(ft) = &self.filter_type {
            args.push("--filter_type".to_string());
            args.push(ft.clone());
        }

        if let Some(fc) = self.filter_cutoff {
            args.push("--filter_cutoff".to_string());
            args.push(fc.to_string());
            if let Some(fc2) = self.filter_cutoff2 {
                args.push(fc2.to_string());
            }
        }

        if let Some(fo) = self.filter_order {
            args.push("--filter_order".to_string());
            args.push(fo.to_string());
        }

        if let Some(custom) = &self.custom_args {
            let custom_parts: Vec<&str> = custom.split_whitespace().collect();
            for part in custom_parts {
                args.push(part.to_string());
            }
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
    address: RwLock<Option<String>>,
    arguments: RwLock<FtioArguments>,
    pub server_logs: Arc<RwLock<Vec<String>>>,
}

impl FtioClient {
    pub fn new() -> Self {
        Self {
            context: Arc::new(zmq::Context::new()),
            address: RwLock::new(None),
            arguments: RwLock::new(FtioArguments::default()),
            server_logs: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn get_port(&self) -> Option<String> {
        let address = self.address.read().unwrap();
        let addr = address.as_ref()?;
        addr.rsplit(':').next().map(|port| port.to_string())
    }

    pub fn get_logs(&self) -> Vec<String> {
        self.server_logs.read().unwrap().clone()
    }

    pub fn get_arguments(&self) -> std::sync::RwLockReadGuard<'_, FtioArguments> {
        self.arguments.read().unwrap()
    }

    pub fn set_arguments(&self, new_args: FtioArguments) {
        let mut args = self.arguments.write().unwrap();
        *args = new_args;
    }

    pub fn set_address(&self, addr: &str) {
        let mut address = self.address.write().unwrap();
        *address = Some(addr.to_string());
    }

    pub fn send_receive_modified(
        &self,
        args: FtioArguments,
        metrics: HashMap<String, serde_json::Value>,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let socket = self.context.socket(zmq::REQ)?;
        socket.set_rcvtimeo(1000)?;
        socket.set_sndtimeo(1000)?;
        let address = self.address.read().unwrap();
        if let Some(addr) = address.as_ref() {
            socket.connect(addr)?;
        } else {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "FTIO client address not set",
            )));
        }

        let payload = serde_json::json!({
            "argv": args.to_args(),
            "metrics": { "metrics": metrics },
            "disable_parallel": true
        });

        let mut buf = Vec::new();
        rmp_serde::encode::write(&mut buf, &payload)?;

        //println!("Sending {} bytes to FTIO server", buf.len());
        socket.send(buf, 0)?;

        let reply = socket.recv_bytes(0)?;
        Ok(reply)
    }

    pub fn send_receive(&self, export: TraceExport) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let socket = self.context.socket(zmq::REQ)?;
        socket.set_rcvtimeo(1000)?;
        socket.set_sndtimeo(1000)?;
        let address = self.address.read().unwrap();
        if let Some(addr) = address.as_ref() {
            socket.connect(addr)?;
        } else {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "FTIO client address not set",
            )));
        }

        let args = self.get_arguments();
        let payload = serde_json::json!({
            "argv": args.to_args(),
            "metrics": export,
            "disable_parallel": false
        });

        let mut buf = Vec::new();
        rmp_serde::encode::write(&mut buf, &payload)?;

        //println!("Sending {} bytes to FTIO server", buf.len());
        socket.send(buf, 0)?;

        let reply = socket.recv_bytes(0)?;
        Ok(reply)
    }

    pub fn ping_server(&self) -> bool {
        let socket = match self.context.socket(zmq::REQ) {
            Ok(s) => s,
            Err(e) => {
                log::error!("Failed to create REQ socket: {}", e);
                return false;
            }
        };
        socket.set_rcvtimeo(1000).unwrap();
        socket.set_sndtimeo(1000).unwrap();
        let address = self.address.read().unwrap();
        if let Some(addr) = address.as_ref() {
            socket.connect(addr).unwrap();
        } else {
            return false;
        }

        if socket.send("ping".as_bytes(), 0).is_err() {
            return false;
        }

        match socket.recv_bytes(0) {
            Ok(reply) => reply == b"pong",
            _ => false,
        }
    }

    pub fn send_new_address(&self, new_port: &str) -> Result<(), Box<dyn std::error::Error>> {
        let socket = self.context.socket(zmq::REQ)?;
        socket.set_rcvtimeo(1000)?;
        socket.set_sndtimeo(1000)?;

        let current_address = {
            let address = self.address.read().unwrap();
            address.clone()
        };

        let addr = current_address.ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::Other, "FTIO client address not set")
        })?;

        socket.connect(&addr)?;

        let new_addr = format!("tcp://127.0.0.1:{}", new_port);
        let msg = format!("New Address: {}", new_addr);
        socket.send(msg.as_bytes(), 0)?;

        match socket.recv_bytes(0) {
            Ok(reply) if reply == b"Address updated" => {
                let mut address = self.address.write().unwrap();
                *address = Some(new_addr);
                Ok(())
            }
            _ => Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Failed to update address on FTIO server",
            ))),
        }
    }
}
