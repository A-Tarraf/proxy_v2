use std::{
    borrow::BorrowMut,
    collections::HashMap,
    error::Error,
    fs::{remove_file, File, OpenOptions},
    io::{Seek, SeekFrom},
    os::unix::prelude::FileExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use serde::{Deserialize, Serialize};

use crate::{
    proxy_common::{check_prefix_dir, list_files_with_ext_in, unix_ts, ProxyErr},
    proxywireprotocol::{CounterSnapshot, CounterValue, JobDesc, JobProfile},
};

#[derive(Serialize, Deserialize)]
struct TraceHeader {
    desc: JobDesc,
}

#[derive(Serialize, Deserialize)]
struct TraceFrame {
    ts: u64,
    counters: Vec<CounterValue>,
}

#[allow(unused)]
struct TraceState {
    size: u64,
    lastprof: Option<Vec<CounterValue>>,
    lastwrite: u64,
    path: PathBuf,
}

impl TraceState {
    fn open(&self, create: bool) -> Result<File, std::io::Error> {
        if create {
            OpenOptions::new()
                .read(true)
                .append(true)
                .create(true)
                .open(&self.path)
        } else {
            OpenOptions::new().read(true).append(true).open(&self.path)
        }
    }

    fn len(&self) -> Result<u64, Box<dyn Error>> {
        let fd = self.open(false)?;
        Ok(fd.metadata()?.len())
    }

    fn desc(&mut self) -> Result<JobDesc, Box<dyn Error>> {
        let mut fd = self.open(false)?;
        let (data, _) = Self::read_frame_at(&mut fd, 0)?;
        let desc: JobDesc = serde_json::from_slice(&data)?;
        Ok(desc)
    }

    fn offset_of_last_frame_start(fd: &File) -> Result<u64, Box<dyn Error>> {
        /* We do -1 as we do not want the last NULL */
        let mut position = fd.metadata()?.len() - 1;

        while position > 0 {
            let read_size = std::cmp::min(1024, position) as usize;
            let mut buffer = vec![0; read_size];

            position -= read_size as u64;

            fd.read_exact_at(&mut buffer, position)?;

            if let Some(idx) = buffer.iter().rposition(|&b| b == 0) {
                return Ok(position + idx as u64);
            }
        }

        Err(ProxyErr::newboxed(
            "No null found in tracefile when scanning for last frame",
        ))
    }

    fn read_last(&mut self) -> Result<Option<TraceFrame>, Box<dyn Error>> {
        let mut fd = self.open(false)?;
        let off = Self::offset_of_last_frame_start(&fd)?;
        if off == (fd.metadata()?.len() - 1) {
            /* This is an empty frame */
            return Ok(None);
        }
        let (data, _) = Self::read_frame_at(&mut fd, off + 1)?;
        let frame: TraceFrame = serde_json::from_slice(&data)?;
        Ok(Some(frame))
    }

    fn read_frame_at(fd: &mut File, off: u64) -> Result<(Vec<u8>, u64), Box<dyn Error>> {
        let mut data: Vec<u8> = Vec::new();
        let mut current_offset = off;

        loop {
            let mut buff: [u8; 1024] = [0; 1024];
            let len = fd.read_at(&mut buff, current_offset)?;

            if len == 0 {
                return Ok((data, current_offset));
            }

            for c in buff.iter().take(len) {
                current_offset += 1;
                if *c == 0 {
                    return Ok((data, current_offset));
                } else {
                    data.push(*c);
                }
            }
        }
    }

    fn push(&mut self, counters: Vec<CounterValue>) -> Result<(), Box<dyn Error>> {
        let frame = TraceFrame {
            ts: unix_ts(),
            counters,
        };

        let mut fd = self.open(false)?;

        serde_json::to_writer(&fd, &frame)?;

        let endoff = fd.stream_position()?;
        fd.write_at(&[0x0], endoff)?;

        self.lastwrite = unix_ts();
        self.size = fd.metadata()?.len();

        Ok(())
    }

    fn read_all(&mut self) -> Result<(JobDesc, Vec<TraceFrame>), Box<dyn Error>> {
        let mut frames = Vec::new();

        let mut fd = self.open(false)?;

        /* First frame is the desc */
        let (mut data, mut current_offset) = Self::read_frame_at(&mut fd, 0)?;
        let desc: JobDesc = serde_json::from_slice(&data)?;

        loop {
            (data, current_offset) = Self::read_frame_at(&mut fd, current_offset)?;

            if data.is_empty() {
                return Ok((desc, frames));
            }

            /* Full frame */
            let frame: TraceFrame = serde_json::from_slice(&data)?;
            frames.push(frame);

            data.clear();
        }
    }

    fn new(path: &Path, job: &JobDesc) -> Result<TraceState, Box<dyn Error>> {
        let ret = TraceState {
            size: 0,
            lastprof: None,
            lastwrite: 0,
            path: path.to_path_buf(),
        };

        let mut fd = ret.open(true)?;

        // First thing save the jobdesc
        serde_json::to_writer(&fd, &job)?;
        // And the first frame marker
        let off = fd.stream_position()?;
        fd.write_at(&[0x00], off)?;

        Ok(ret)
    }

    fn from(path: &Path) -> Result<TraceState, Box<dyn Error>> {
        let mut ret = TraceState {
            size: 0,
            lastprof: None,
            lastwrite: 0,
            path: path.to_path_buf(),
        };
        let lastframe = ret.read_last()?;

        ret.size = ret.len()?;

        if let Some(f) = lastframe {
            ret.lastwrite = f.ts;
            ret.lastprof = Some(f.counters)
        }

        Ok(ret)
    }
}

pub(crate) struct Trace {
    desc: JobDesc,
    state: Mutex<TraceState>,
    done: RwLock<bool>,
}

impl Trace {
    fn new_from_file(file: &String) -> Result<Trace, Box<dyn Error>> {
        let path = Path::new(&file);
        let mut state = TraceState::from(path)?;
        let mut desc = state.desc()?;
        /* Assume end time is the last profile write ~1 sec exact */
        if state.lastwrite != 0 {
            desc.end_time = state.lastwrite;
        }
        Ok(Trace {
            desc,
            state: Mutex::new(state),
            done: RwLock::new(false),
        })
    }

    fn name(prefix: &Path, desc: &JobDesc) -> PathBuf {
        let mut path = prefix.to_path_buf();
        path.push(format!("{}.trace", desc.jobid));
        path
    }

    fn new(prefix: &Path, desc: &JobDesc) -> Result<Trace, Box<dyn Error>> {
        let path = Trace::name(prefix, desc);
        if path.exists() {
            return Err(ProxyErr::newboxed(format!(
                "Cannot create trace it already exists at {}",
                path.to_string_lossy(),
            )));
        }

        let state = TraceState::new(&path, desc)?;

        Ok(Trace {
            desc: desc.clone(),
            state: Mutex::new(state),
            done: RwLock::new(false),
        })
    }

    pub(crate) fn desc(&self) -> &JobDesc {
        &self.desc
    }

    pub(crate) fn path(&self) -> String {
        self.state
            .lock()
            .unwrap()
            .path
            .to_string_lossy()
            .to_string()
    }

    pub(crate) fn push(&self, profile: JobProfile) -> Result<(), Box<dyn Error>> {
        let done = self.done.read().unwrap();

        if *done {
            return Err(ProxyErr::newboxed("Job is done"));
        }

        let values: Vec<CounterValue> = profile.counters.iter().map(|v| v.value()).collect();
        self.state.lock().unwrap().push(values)?;

        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct TraceInfo {
    desc: JobDesc,
    size: u64,
    lastwrite: u64,
}

#[derive(Serialize)]
pub(crate) struct TraceRead {
    info: TraceInfo,
    frames: Vec<TraceFrame>,
}

impl TraceInfo {
    pub(crate) fn new(trace: &Trace) -> TraceInfo {
        let infos = trace.state.lock().unwrap();
        TraceInfo {
            desc: trace.desc.clone(),
            size: infos.size,
            lastwrite: infos.lastwrite,
        }
    }
}

pub(crate) struct TraceView {
    prefix: PathBuf,
    traces: RwLock<HashMap<String, Arc<Trace>>>,
}

impl TraceView {
    fn load_existing_traces(
        prefix: &PathBuf,
    ) -> Result<HashMap<String, Arc<Trace>>, Box<dyn Error>> {
        let mut ret: HashMap<String, Arc<Trace>> = HashMap::new();

        let files = list_files_with_ext_in(prefix, "trace")?;

        for f in files.iter() {
            match Trace::new_from_file(f) {
                Ok(t) => {
                    ret.insert(t.desc.jobid.to_string(), Arc::new(t));
                }
                Err(e) => {
                    log::error!("Failed to load trace from {} : {}", f, e);
                }
            }
        }

        Ok(ret)
    }

    pub(crate) fn list(&self) -> Vec<TraceInfo> {
        self.traces
            .read()
            .unwrap()
            .values()
            .map(|v| TraceInfo::new(v))
            .collect()
    }

    pub(crate) fn clear(&self, desc: &JobDesc) -> Result<(), Box<dyn Error>> {
        self.done(desc)?;
        let path = Trace::name(&self.prefix, desc);
        if path.is_file() {
            remove_file(path)?;
        }
        Ok(())
    }

    pub(crate) fn read(
        &self,
        jobid: String,
        filter: Option<String>,
    ) -> Result<TraceRead, ProxyErr> {
        let ht = self.traces.read().unwrap();

        if let Some(trace) = ht.get(&jobid) {
            let (_, frames) = trace.state.lock().unwrap().read_all()?;

            return Ok(TraceRead {
                info: TraceInfo::new(trace),
                frames,
            });
        }

        Err(ProxyErr::new(format!("No such trace id {}", jobid)))
    }

    pub(crate) fn get(&self, jobdesc: &JobDesc) -> Result<Arc<Trace>, Box<dyn Error>> {
        let mut ht = self.traces.write().unwrap();

        let trace = match ht.get(&jobdesc.jobid) {
            Some(v) => v.clone(),
            None => {
                let trace = Trace::new(&self.prefix, jobdesc)?;
                let ret = Arc::new(trace);
                ht.insert(jobdesc.jobid.to_string(), ret.clone());
                ret
            }
        };

        Ok(trace)
    }

    pub(crate) fn done(&self, job: &JobDesc) -> Result<(), Box<dyn Error>> {
        if let Some(j) = self.traces.write().unwrap().get_mut(&job.jobid) {
            *j.done.write().unwrap() = true;
        }

        self.traces.write().unwrap().remove(&job.jobid);
        Ok(())
    }

    pub(crate) fn new(prefix: &PathBuf) -> Result<TraceView, Box<dyn Error>> {
        let prefix = check_prefix_dir(prefix, "traces")?;
        let traces = RwLock::new(Self::load_existing_traces(&prefix)?);
        Ok(TraceView { prefix, traces })
    }
}
