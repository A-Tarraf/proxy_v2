use clap::Parser;
use std::{
    collections::HashMap,
    error::Error,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::to_string_pretty;

use std::sync::Arc;

mod proxywireprotocol;
mod trace;

use trace::TraceExport;

mod proxy_common;
use proxy_common::{derivate_time_serie, ProxyErr};

use colored::Colorize;

use std::fs::File;
use std::io::Write;

mod exporter;
mod extrap;
mod profiles;
mod scrapper;
mod systemmetrics;
use exporter::ExporterFactory;

use rayon::iter::*;

use crate::{proxy_common::offset_time_serie, trace::TraceView};

#[derive(Parser)]
struct Cli {
    #[arg(short, long)]
    /// Path to the profile directory (default ~/.proxyprofile/)
    profile_path: Option<PathBuf>,
    /// List traces only
    #[arg(short, long, default_value_t = false)]
    list: bool,
    /// The name of the job to export
    #[arg(short, long)]
    job: Option<String>,
    /// Export all jobs
    #[arg(short, long, default_value_t = false)]
    all_jobs: bool,
    /// Where to write (cannot be used with -a)
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Export a trace for the given job(s)
    #[arg(short, long, default_value_t = false)]
    export_trace: bool,
    /// Export a model for the given job(s)
    #[arg(short, long, default_value_t = false)]
    gen_model: bool,
}

struct TraceExporter {
    factory: Arc<ExporterFactory>,
}

impl TraceExporter {
    fn new(path: &Path) -> Result<TraceExporter, ProxyErr> {
        let factory = ExporterFactory::new(path.to_path_buf(), false, 1024 * 1024 * 32)?;
        Ok(TraceExporter { factory })
    }

    fn list(&self) -> Result<(), ProxyErr> {
        let traces = self.factory.trace_store.list();

        for tr in traces {
            println!("JOB: {}", tr.desc.jobid.red());
            println!("{}", to_string_pretty(&tr).unwrap());
        }

        Ok(())
    }

    fn export(&self, from: &String, to: &Option<PathBuf>) -> Result<(), Box<dyn Error>> {
        /* Get infos */
        let infos = self.factory.trace_store.infos(from)?;

        let output = if let Some(out) = to {
            out.clone()
        } else {
            Path::new(&format!(
                "./{}.{}procs.trace.json",
                infos.desc.command.replace("./", "").trim(),
                infos.desc.size
            ))
            .to_path_buf()
        };
        println!("Creating {}", output.to_string_lossy());

        if output.exists() {
            return Err(ProxyErr::newboxed(format!(
                "Output file {} already exists",
                output.to_string_lossy()
            )));
        }

        log::info!("Creating {}", output.to_string_lossy());

        let file = File::create(output)?;

        let export = TraceExport::new(infos, &self.factory.trace_store)?;

        serde_json::to_writer(file, &export)?;

        Ok(())
    }

    fn extrap(&self, from: &String, to: &Option<PathBuf>) -> Result<(), Box<dyn Error>> {
        /* Get infos */
        let infos = self.factory.trace_store.infos(from)?;

        let output = if let Some(out) = to {
            out.clone()
        } else {
            Path::new(&format!(
                "./{}.model.jsonl",
                infos.desc.command.replace("./", "").trim()
            ))
            .to_path_buf()
        };

        if output.exists() {
            return Err(ProxyErr::newboxed(format!(
                "Output file {} already exists",
                output.to_string_lossy()
            )));
        }

        if let Ok(jsonl) = self.factory.profile_store.get_jsonl(&infos.desc.command) {
            let mut outf = File::create(output)?;
            outf.write_all(jsonl.as_bytes())?;
        }

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

    if !profile_dir.is_dir() {
        return Err(ProxyErr::newboxed(format!(
            "{} is not a directory",
            profile_dir.to_string_lossy()
        )));
    }

    let tv = TraceExporter::new(&profile_dir)?;

    if args.list {
        /* List traces */
        tv.list()?;
        return Ok(());
    }

    /* From there we need a jobid */
    let mut jobs: Vec<String> = Vec::new();

    if args.all_jobs {
        for d in tv.factory.trace_store.list().iter() {
            jobs.push(d.desc.jobid.clone());
        }
    } else if let Some(job) = args.job {
        jobs.push(job);
    }

    if args.export_trace && args.gen_model && args.output.is_some() {
        return Err(ProxyErr::newboxed(
            "Exporting both traces and profiles is only possible with auto-naming (no -o)",
        ));
    }

    if jobs.is_empty() {
        return Err(ProxyErr::newboxed("No job to process use either -j or -a"));
    }

    jobs.par_iter().for_each(|j| {
        if args.export_trace {
            if let Err(e) = tv.export(j, &args.output) {
                println!("Failed to generate trace for {} : {}", j, e);
            }
        }

        if args.gen_model {
            if let Err(e) = tv.extrap(j, &args.output) {
                println!("Failed to generate model for {} : {}", j, e);
            }
        }
    });

    Ok(())
}
