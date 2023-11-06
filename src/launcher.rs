use std::ffi::{OsStr, OsString};
use std::process::exit;
use std::{error::Error, path::PathBuf};

mod proxy_common;
use proxy_common::init_log;
use proxy_common::{list_files_with_ext_in, ProxyErr};

use std::path::Path;

use clap::Parser;
use std::env::current_exe;
use std::process::{Command, Stdio};

use std::fs;

enum Exporter {
    Preload { name: String, path: PathBuf },
}

impl Exporter {
    fn newpreload(path: PathBuf) -> Exporter {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("libmetricproxy-exporter-"));
        assert!(name.ends_with(".so"));

        let name = name
            .replace("libmetricproxy-exporter-", "")
            .replace(".so", "");

        log::debug!("Located Exporter {} in {}", name, path.to_string_lossy());

        Exporter::Preload { name, path }
    }

    fn name(&self) -> String {
        match self {
            Exporter::Preload { name, path: _ } => name.to_string(),
        }
    }
}

struct ExporterList {
    libdir: PathBuf,
    exporters: Vec<Exporter>,
}

impl ExporterList {
    fn generate_preloads(&self, filter: &[String]) -> String {
        self.exporters
            .iter()
            .filter_map(|v| {
                if !filter.contains(&v.name()) && !filter.is_empty() {
                    return None;
                }
                match v {
                    Exporter::Preload { name: _, path } => Some(path.to_string_lossy().to_string()),
                    _ => None,
                }
            })
            .collect::<Vec<String>>()
            .join(":")
    }

    fn getpaths() -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
        let current_exe = current_exe()?;
        let current_exe = fs::canonicalize(current_exe)?;
        log::trace!("Exe Path {}", current_exe.to_string_lossy());
        if let Some(bindir) = current_exe.parent() {
            log::trace!("bindir {}", bindir.to_string_lossy());
            if let Some(prefix) = bindir.parent() {
                log::trace!("prefix {}", prefix.to_string_lossy());
                let libdir = prefix.join("lib");
                if !libdir.is_dir() {
                    return Err(ProxyErr::newboxed(format!(
                        "{} is not a directory, failed to locate library directory",
                        libdir.to_string_lossy(),
                    )));
                }
                return Ok((bindir.to_path_buf(), libdir.to_path_buf()));
            }
        }

        Err(ProxyErr::newboxed("Failed to infer binary prefix"))
    }

    fn new() -> Result<ExporterList, Box<dyn Error>> {
        let (bindir, libdir) = ExporterList::getpaths()?;
        log::debug!("Libdir is {}", libdir.to_string_lossy());

        let exporters: Vec<Exporter> = list_files_with_ext_in(&libdir, "so")?
            .iter()
            .filter(|v| v.contains("libmetricproxy-exporter"))
            .map(|v| Exporter::newpreload(Path::new(v).to_path_buf()))
            .collect();

        Ok(ExporterList { libdir, exporters })
    }

    fn exists(&self, exporter: &String) -> bool {
        let names: Vec<String> = self.exporters.iter().map(|v| v.name()).collect();
        names.contains(exporter)
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// List detected exporters
    #[arg(short, long, default_value_t = false)]
    exporterlist: bool,
    /// List of exporters to activate
    #[arg(short, long, value_delimiter = ',')]
    exporter: Option<Vec<String>>,
    /// Optionnal JOBID (MPI/SLURM may generate one automatically)
    #[arg(short, long)]
    jobid: Option<String>,
    /// Optionnal path to proxy UNIX Socket
    #[arg(short, long)]
    unixsocket: Option<String>,
    /// A command to run (passed after --)
    #[arg(last = true)]
    command: Vec<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    init_log();

    let exs = ExporterList::new()?;
    let args = Args::parse();

    if args.exporterlist {
        for v in exs.exporters.iter() {
            println!(" - {}", v.name());
        }
        exit(0);
    }

    if args.command.is_empty() {
        log::error!("You must supply a command after -- to be run.");
        exit(1);
    }

    let mut preloads: Option<String> = None;

    if let Some(exporters) = args.exporter.clone() {
        for e in exporters.iter().as_ref() {
            if !exs.exists(e) {
                log::error!("No such exporter {}", e);
                exit(1);
            }
        }

        preloads = Some(exs.generate_preloads(&exporters));
    } else {
        preloads = Some(exs.generate_preloads(&[]));
    }

    /* Prepare to Run the command  */
    let mut cmd = Command::new(args.command[0].clone());

    /* Handle env preloads */
    if let Some(pr) = preloads {
        cmd.env("LD_PRELOAD", pr);
    }

    /* Handle JobID */
    if let Some(jobid) = args.jobid {
        cmd.env("PROXY_JOB_ID", jobid);
    }

    /* Handle Proxy Socket */
    if let Some(unix) = args.unixsocket {
        cmd.env("PROXY_PATH", unix);
    }

    /* Forward arguments */
    let args: Vec<OsString> = args.command[1..]
        .iter()
        .cloned()
        .map(OsString::from)
        .collect();

    for arg in &args {
        cmd.arg(arg);
    }

    // Run the command
    let status = cmd.status()?;

    if !status.success() {
        exit(status.code().unwrap_or(1));
    }

    Ok(())
}
