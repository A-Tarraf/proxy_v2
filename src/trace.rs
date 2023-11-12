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
    proxywireprotocol::{CounterSnapshot, CounterType, CounterValue, JobDesc, JobProfile},
};

#[derive(Serialize, Deserialize)]
struct TraceHeader {
    desc: JobDesc,
}

#[derive(Serialize, Deserialize)]
struct TraceCounter {
    id: u64,
    value: CounterType,
}

#[derive(Serialize, Deserialize, Clone)]
struct TraceCounterMetadata {
    id: u64,
    name: String,
    doc: String,
}

#[derive(Serialize, Deserialize)]
enum TraceFrame {
    Desc {
        ts: u64,
        desc: JobDesc,
    },
    CounterMetadata {
        ts: u64,
        metadata: TraceCounterMetadata,
    },
    Counters {
        ts: u64,
        counters: Vec<TraceCounter>,
    },
}

impl TraceFrame {
    fn ts(&self) -> u64 {
        match *self {
            TraceFrame::Desc { ts, desc: _ } => ts,
            TraceFrame::CounterMetadata { ts, metadata: _ } => ts,
            TraceFrame::Counters { ts, counters: _ } => ts,
        }
    }

    fn desc(self) -> Result<JobDesc, ProxyErr> {
        match self {
            TraceFrame::Desc { ts: _, desc } => Ok(desc),
            _ => Err(ProxyErr::new("Frame is not a jobdesc")),
        }
    }
}

#[allow(unused)]
struct TraceState {
    size: u64,
    lastwrite: u64,
    path: PathBuf,
    counters: HashMap<String, TraceCounterMetadata>,
    current_counter_id: u64,
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

        if let Some(frame) = data {
            return Ok(frame.desc()?);
        }

        Err(ProxyErr::newboxed(
            "First frame of the trace is not a trace description",
        ))
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
        Ok(data)
    }

    fn read_frame_at(fd: &mut File, off: u64) -> Result<(Option<TraceFrame>, u64), Box<dyn Error>> {
        let mut data: Vec<u8> = Vec::new();
        let mut current_offset = off;

        loop {
            let mut buff: [u8; 1024] = [0; 1024];
            let len = fd.read_at(&mut buff, current_offset)?;

            if len == 0 {
                return Ok((None, current_offset));
            }

            for c in buff.iter().take(len) {
                current_offset += 1;
                if *c == 0 {
                    let frame: TraceFrame = serde_json::from_slice(&data)?;
                    return Ok((Some(frame), current_offset));
                } else {
                    data.push(*c);
                }
            }
        }
    }

    fn check_counter(&mut self, counters: &Vec<CounterSnapshot>) -> Vec<TraceFrame> {
        let mut ret: Vec<TraceFrame> = Vec::new();
        for c in counters.iter() {
            if !self.counters.contains_key(&c.name) {
                let metadata = TraceCounterMetadata {
                    id: self.current_counter_id,
                    name: c.name.to_string(),
                    doc: c.doc.to_string(),
                };

                self.current_counter_id += 1;

                self.counters.insert(c.name.to_string(), metadata.clone());

                let frame = TraceFrame::CounterMetadata {
                    ts: unix_ts(),
                    metadata,
                };

                ret.push(frame)
            }
        }
        ret
    }

    fn counter_id(&self, counter: &CounterSnapshot) -> Option<u64> {
        if let Some(c) = self.counters.get(&counter.name) {
            return Some(c.id);
        }

        None
    }

    fn do_write_frame(fd: &mut File, frame: TraceFrame) -> Result<(), Box<dyn Error>> {
        let mut buff: Vec<u8> = serde_json::to_vec(&frame)?;
        buff.push(0x0);
        let endoff: u64 = fd.stream_position()?;
        fd.write_at(&buff, endoff)?;

        Ok(())
    }

    fn write_frame(&self, frame: TraceFrame) -> Result<(), Box<dyn Error>> {
        let mut fd = self.open(false)?;

        Self::do_write_frame(&mut fd, frame)?;

        Ok(())
    }

    fn write_frames(&mut self, frames: Vec<TraceFrame>) -> Result<(), Box<dyn Error>> {
        if frames.is_empty() {
            return Ok(());
        }

        let mut fd = self.open(false)?;

        for f in frames {
            Self::do_write_frame(&mut fd, f)?;
        }

        self.lastwrite = unix_ts();
        self.size = fd.metadata()?.len();

        Ok(())
    }

    fn push(&mut self, counters: Vec<CounterSnapshot>) -> Result<(), Box<dyn Error>> {
        let new_counters: Vec<TraceFrame> = self.check_counter(&counters);

        self.write_frames(new_counters)?;

        let counters = counters
            .iter()
            .map(|v| TraceCounter {
                id: self.counter_id(v).unwrap(),
                value: v.ctype.clone(),
            })
            .collect();

        let frame = TraceFrame::Counters {
            ts: unix_ts(),
            counters,
        };

        self.write_frame(frame)?;

        Ok(())
    }

    fn read_all(&mut self) -> Result<(JobDesc, Vec<TraceFrame>), Box<dyn Error>> {
        let mut frames = Vec::new();

        let mut fd = self.open(false)?;

        /* First frame is the desc */
        let (mut frame, mut current_offset) = Self::read_frame_at(&mut fd, 0)?;

        if frame.is_none() {
            return Err(ProxyErr::newboxed("Failed to read first frame"));
        }

        let desc = frame.unwrap().desc()?;

        loop {
            (frame, current_offset) = Self::read_frame_at(&mut fd, current_offset)?;

            if frame.is_none() {
                return Ok((desc, frames));
            }

            /* Full frame */
            frames.push(frame.unwrap());
        }
    }

    fn new(path: &Path, job: &JobDesc) -> Result<TraceState, Box<dyn Error>> {
        let ret = TraceState {
            size: 0,
            lastwrite: 0,
            path: path.to_path_buf(),
            counters: HashMap::new(),
            current_counter_id: 0,
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
            lastwrite: 0,
            path: path.to_path_buf(),
            counters: HashMap::new(),
            current_counter_id: 0,
        };
        let lastframe = ret.read_last()?;

        ret.size = ret.len()?;

        if let Some(f) = lastframe {
            ret.lastwrite = f.ts();
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

        self.state.lock().unwrap().push(profile.counters)?;

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
            log::error!("Removing {}", path.to_string_lossy());
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
