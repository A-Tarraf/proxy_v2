use std::ffi::OsString;
use std::process::exit;
use std::{error::Error, path::PathBuf};

mod proxy_common;
use proxy_common::init_log;
use proxy_common::{list_files_with_ext_in, ProxyErr};

use std::path::Path;

use clap::Parser;
use std::env::{self, current_exe};
use std::process::Command;

use std::{fs, os};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();

    if !args.contains(&"--".to_string()) {
        return Err(ProxyErr::newboxed(
            "Please put -- in your command to help indentify the target command",
        ));
    }

    let mut seen_separator = false;

    let args: Vec<String> = args.into_iter().fold(Vec::new(), |mut acc, x| {
        if !seen_separator {
            if x == "--" {
                seen_separator = true;
            }
        } else {
            acc.push(x);
        }
        acc
    });

    let srun = match env::var("SRUN") {
        Ok(v) => v,
        Err(_) => {
            return Err(ProxyErr::newboxed(
                "Please set path to true srun in SRUN environment variable",
            ))
        }
    };

    let mut command = vec![srun];
    command.extend(["--comment".to_string(), args.join(" ")]);
    command.extend(args);

    /* Prepare to Run the command  */
    let mut cmd = Command::new(command[0].clone());
    cmd.args(&command[1..]);

    // Run the command
    let status = cmd.status()?;

    if !status.success() {
        exit(status.code().unwrap_or(1));
    }

    Ok(())
}
