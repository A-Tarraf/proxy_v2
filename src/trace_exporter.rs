use clap::Parser;
use std::{
    collections::HashMap,
    error::Error,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::to_string_pretty;

mod proxywireprotocol;
mod trace;

use trace::{TraceInfo, TraceView};

mod proxy_common;
use proxy_common::{derivate_time_serie, ProxyErr};

use colored::Colorize;

use std::fs::File;

#[derive(Parser)]
struct Cli {
    #[arg(short, long)]
    profile_path: Option<PathBuf>,
    #[arg(short, long, default_value_t = false)]
    list: bool,
    #[arg(short, long)]
    export_trace: Option<String>,
    #[arg(short, long)]
    gen_model: Option<String>,
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Serialize)]
struct TraceExport {
    infos: TraceInfo,
    metrics: HashMap<String, Vec<(u64, f64)>>,
}

impl TraceExport {
    fn new(infos: TraceInfo) -> TraceExport {
        TraceExport {
            infos,
            metrics: HashMap::new(),
        }
    }

    fn set(&mut self, name: String, values: Vec<(u64, f64)>) -> Result<(), ProxyErr> {
        self.metrics.insert(name, values);
        Ok(())
    }
}

struct ProfileExporter {
    traceview: TraceView,
}

impl ProfileExporter {
    fn new(path: &PathBuf) -> Result<ProfileExporter, ProxyErr> {
        let traceview = TraceView::new(path)?;
        Ok(ProfileExporter { traceview })
    }

    fn list(&self) -> Result<(), ProxyErr> {
        let traces = self.traceview.list();

        for tr in traces {
            println!("JOB: {}", tr.desc.jobid.red());
            println!("{}", to_string_pretty(&tr).unwrap());
        }

        Ok(())
    }

    fn export(&self, from: String, to: &Option<PathBuf>) -> Result<(), Box<dyn Error>> {
        let output = if let Some(out) = to {
            out
        } else {
            return Err(ProxyErr::newboxed("No output file given"));
        };

        if output.exists() {
            return Err(ProxyErr::newboxed(format!(
                "Output file {} already exists",
                output.to_string_lossy()
            )));
        }

        let file = File::create(output)?;

        /* Get infos */
        let infos = self.traceview.infos(&from)?;

        /* Get metrics */
        let metrics = self.traceview.metrics(&from)?;

        let mut export = TraceExport::new(infos);

        /* Now for all metrics we get the data and its derivate and we store in the output hashtable */
        for m in metrics {
            let data = self.traceview.plot(&from, m.clone())?;
            /* Derivate the data  */
            let deriv = derivate_time_serie(data.clone());

            export.set(m.clone(), data)?;
            export.set(format!("deriv__{}", m), deriv)?;
        }

        serde_json::to_writer(file, &export)?;

        Ok(())
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Cli::parse();

    let profile_dir = if let Some(p) = args.profile_path {
        p.clone()
    } else {
        let mut profile_prefix = dirs::home_dir().unwrap();
        profile_prefix.push(".proxyprofiles");
        profile_prefix.to_path_buf()
    };

    let tv = ProfileExporter::new(&profile_dir)?;

    if args.list {
        /* List traces */
        tv.list()?;
        return Ok(());
    }

    if let Some(jobid) = args.export_trace {
        tv.export(jobid, &args.output)?;
        return Ok(());
    }

    Ok(())
}
