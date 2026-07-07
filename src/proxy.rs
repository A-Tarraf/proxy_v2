use std::error::Error;
use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use crate::proxy_common::unix_ts;
use crate::proxywireprotocol::JobDesc;

use super::exporter::{Exporter, ExporterFactory};
use super::proxy_common::ProxyErr;

use super::proxywireprotocol::ProxyCommand;

/********************
 * UNIX DATA SERVER *
 ********************/

pub(crate) struct UnixProxy {
    listener: UnixListener,
    factory: Arc<ExporterFactory>,
}

struct PerClientState {
    factory: Arc<ExporterFactory>,
    job_exporter: Option<Arc<Exporter>>,
    job_desc: Option<JobDesc>,
    /// True once this connection pushed an "mpi___" Desc — it is the MPI
    /// exporter of one rank (strace and other exporters use other prefixes).
    is_mpi_rank: bool,
    /// Job the rank was counted for (per-job counts follow malleable jobs)
    rank_jobid: Option<String>,
}

impl Drop for PerClientState {
    fn drop(&mut self) {
        /* Drop-based so the rank count stays correct even when the client
         * disconnects abruptly and handle_client exits early with an error */
        if self.is_mpi_rank {
            self.factory.mpi_ranks.fetch_sub(1, Ordering::Relaxed);
        }
        if let Some(jobid) = &self.rank_jobid {
            let mut ht = self.factory.job_mpi_ranks.lock().unwrap();
            if let Some(c) = ht.get_mut(jobid) {
                *c -= 1;
                if *c <= 0 {
                    ht.remove(jobid);
                }
            }
        }
    }
}

impl UnixProxy {
    fn handle_command(
        per_client_state: &mut PerClientState,
        command: ProxyCommand,
    ) -> Result<(), Box<dyn Error>> {
        log::debug!("{:?}", command);
        match command {
            ProxyCommand::Desc(desc) => {
                if !per_client_state.is_mpi_rank && desc.name.starts_with("mpi___") {
                    per_client_state.is_mpi_rank = true;
                    per_client_state
                        .factory
                        .mpi_ranks
                        .fetch_add(1, Ordering::Relaxed);
                    if let Some(d) = &per_client_state.job_desc {
                        if !d.jobid.is_empty() {
                            per_client_state.rank_jobid = Some(d.jobid.clone());
                            *per_client_state
                                .factory
                                .job_mpi_ranks
                                .lock()
                                .unwrap()
                                .entry(d.jobid.clone())
                                .or_insert(0) += 1;
                        }
                    }
                }
                per_client_state.factory.push(
                    desc.name.as_str(),
                    desc.doc.as_str(),
                    desc.ctype.clone(),
                    per_client_state.job_exporter.clone(),
                )?;
            }
            ProxyCommand::Value(value) => {
                per_client_state.factory.accumulate(
                    value.name.as_str(),
                    value.value,
                    per_client_state.job_exporter.clone(),
                )?;
            }
            ProxyCommand::JobDesc(d) => {
                per_client_state.job_desc = Some(d);

                if let Some(desc) = &mut per_client_state.job_desc {
                    if !desc.jobid.is_empty() {
                        /* No need to start the exporter if the jobid is empty */
                        per_client_state.job_exporter =
                            Some(per_client_state.factory.resolve_job(desc, true));
                    }
                }

                /* The MPI exporter may declare counters before the job: if
                 * this connection is already a counted rank, attribute it to
                 * the job now (per-job counts feed job_mpi_ranks) */
                if per_client_state.is_mpi_rank && per_client_state.rank_jobid.is_none() {
                    if let Some(desc) = &per_client_state.job_desc {
                        if !desc.jobid.is_empty() {
                            per_client_state.rank_jobid = Some(desc.jobid.clone());
                            *per_client_state
                                .factory
                                .job_mpi_ranks
                                .lock()
                                .unwrap()
                                .entry(desc.jobid.clone())
                                .or_insert(0) += 1;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_client(
        factory: Arc<ExporterFactory>,
        mut stream: UnixStream,
    ) -> Result<(), Box<dyn Error>> {
        let mut received_data: Vec<u8> = Vec::new();

        let mut per_client_state = PerClientState {
            factory: factory.clone(),
            job_exporter: None,
            job_desc: None,
            is_mpi_rank: false,
            rank_jobid: None,
        };

        loop {
            let mut buff: [u8; 1024] = [0; 1024];
            let len = stream.read(&mut buff)?;

            if len == 0 {
                break;
            }
            //Check Buffread
            for c in buff.iter().take(len) {
                if *c == 0 {
                    /* Full command */
                    let cmd: ProxyCommand = serde_json::from_slice(&received_data)?;
                    UnixProxy::handle_command(&mut per_client_state, cmd)?;
                    received_data.clear();
                } else {
                    received_data.push(*c);
                }
            }
        }

        /* take(): PerClientState has a Drop impl, so the field cannot be moved out */
        if let Some(mut desc) = per_client_state.job_desc.take() {
            if !desc.jobid.is_empty() {
                /* We set the end Unix TS each time we relax */
                desc.end_time = unix_ts();
                per_client_state.factory.relax_job(&desc)?;
            }
        }

        Ok(())
    }

    pub(crate) fn run(&self) -> Result<(), ProxyErr> {
        for stream in self.listener.incoming() {
            match stream {
                Ok(stream) => {
                    log::debug!("New connection");

                    let factory = self.factory.clone();
                    factory.connected_procs.fetch_add(1, Ordering::Relaxed);

                    // Handle the connection in a new thread.
                    thread::spawn(move || {
                        match UnixProxy::handle_client(factory.clone(), stream) {
                            Ok(_) => {
                                log::debug!("Client left");
                            }
                            Err(e) => {
                                log::error!("Proxy server closing on client : {}", e.to_string());
                            }
                        }
                        factory.connected_procs.fetch_sub(1, Ordering::Relaxed);
                    });
                }
                Err(err) => {
                    log::error!("Error accepting connection: {:?}", err);
                }
            }
        }

        Ok(())
    }

    pub(crate) fn new(
        socket_path: String,
        factory: Arc<ExporterFactory>,
    ) -> Result<UnixProxy, Box<dyn Error>> {
        let path = Path::new(&socket_path);

        if path.exists() {
            std::fs::remove_file(path)
                .or(Err(ProxyErr::new("Failed to remove previous proxy file")))?;
        }

        let listener = UnixListener::bind(path)?;

        let proxy = UnixProxy { listener, factory };

        log::info!("UNIX proxy listening on {}", socket_path);

        Ok(proxy)
    }
}
