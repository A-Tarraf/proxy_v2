use std::{
    collections::HashMap,
    error::Error,
    fs::{remove_file, File, OpenOptions},
    io::{Read, Seek},
    os::unix::prelude::FileExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use serde::{Deserialize, Serialize};
use serde_binary::binary_stream;

use crate::{
    proxy_common::{check_prefix_dir, list_files_with_ext_in, unix_ts, ProxyErr},
    proxywireprotocol::{
        max_f64, min_f64, CounterSnapshot, CounterType, CounterValue, JobDesc, JobProfile,
    },
};

#[derive(Serialize, Deserialize)]
struct TraceHeader {
    desc: JobDesc,
}

#[derive(Serialize, Deserialize, Clone)]
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

#[derive(Serialize, Deserialize, Clone)]
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

    fn mergecounters(first: Vec<TraceCounter>, second: Vec<TraceCounter>) -> Vec<TraceCounter> {
        let mut ret: Vec<TraceCounter> = Vec::new();
        let first = first.iter().map(|v| (v.id, v)).collect::<HashMap<_, _>>();

        for v in second {
            if let Some(prev) = first.get(&v.id) {
                match v.value {
                    CounterType::Counter { value } => match prev.value {
                        CounterType::Counter { value: preval } => ret.push(TraceCounter {
                            id: v.id,
                            value: CounterType::Counter { value },
                        }),
                        CounterType::Gauge { .. } => {}
                    },
                    CounterType::Gauge {
                        min,
                        max,
                        hits,
                        total,
                    } => match prev.value {
                        CounterType::Gauge {
                            min: min2,
                            max: max2,
                            hits: hits2,
                            total: total2,
                        } => ret.push(TraceCounter {
                            id: v.id,
                            value: CounterType::Gauge {
                                min: min_f64(min, min2),
                                max: max_f64(max, max2),
                                hits: hits + hits2,
                                total: total + total2,
                            },
                        }),
                        CounterType::Counter { .. } => {}
                    },
                }
            } else {
                ret.push(v);
            }
        }

        ret
    }

    fn sum(self, other: TraceFrame) -> Result<TraceFrame, ProxyErr> {
        match self {
            TraceFrame::Counters { ts, counters } => match other {
                TraceFrame::Counters {
                    ts: tsb,
                    counters: countersb,
                } => Ok(TraceFrame::Counters {
                    ts: tsb,
                    counters: TraceFrame::mergecounters(counters, countersb),
                }),
                _ => unreachable!("This function must take a counter"),
            },
            _ => unreachable!("This function must take a counter"),
        }
    }

    fn is_counters(&self) -> bool {
        match self {
            TraceFrame::Counters { .. } => true,
            _ => false,
        }
    }

    fn is_desc(&self) -> bool {
        match self {
            TraceFrame::Desc { .. } => true,
            _ => false,
        }
    }

    fn is_metadata(&self) -> bool {
        match self {
            TraceFrame::CounterMetadata { .. } => true,
            _ => false,
        }
    }
}

#[allow(unused)]
struct TraceState {
    size: u64,
    max_size: usize,
    lastwrite: u64,
    path: PathBuf,
    counters: HashMap<String, TraceCounterMetadata>,
    last_value: HashMap<u64, CounterType>,
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

    fn offset_of_last_frame_start(fd: &mut File) -> Result<u64, Box<dyn Error>> {
        let total_size = fd.metadata()?.len();
        let mut offset = 0;
        loop {
            let mut size: [u8; 8] = [0; 8];
            fd.read_exact_at(&mut size, offset).unwrap();
            let size = u64::from_le_bytes(size);

            offset += 8;

            match (offset + size).cmp(&total_size) {
                std::cmp::Ordering::Equal => {
                    return Ok(offset - 8);
                }
                std::cmp::Ordering::Greater => {
                    return Err(ProxyErr::newboxed(
                        "Overrun of the file when scanning for EOF",
                    ));
                }
                _ => {}
            }

            offset += size;
        }
    }

    fn read_last(&mut self) -> Result<Option<TraceFrame>, Box<dyn Error>> {
        let mut fd = self.open(false)?;
        let off = Self::offset_of_last_frame_start(&mut fd)?;
        if off == (fd.metadata()?.len() - 1) {
            /* This is an empty frame */
            return Ok(None);
        }
        let (data, _) = Self::read_frame_at(&mut fd, off)?;
        Ok(data)
    }

    fn read_frame_at(fd: &mut File, off: u64) -> Result<(Option<TraceFrame>, u64), Box<dyn Error>> {
        let mut data: Vec<u8> = Vec::new();
        let mut current_offset = off;

        /* We expect an 8 bytes integer at the start */
        let mut len_data: [u8; 8] = [0; 8];
        if fd.read_at(&mut len_data, current_offset)? == 0 {
            /* EOF */
            return Ok((None, current_offset));
        }
        current_offset += 8;

        let mut left_to_read = u64::from_le_bytes(len_data);

        loop {
            let block_size = if left_to_read < 1024 {
                left_to_read as usize
            } else {
                1024
            };

            let mut buff: [u8; 1024] = [0; 1024];
            let len = fd.read_at(&mut buff[..block_size], current_offset)?;

            for c in buff.iter().take(len) {
                current_offset += 1;
                left_to_read -= 1;

                data.push(*c);

                if left_to_read == 0 {
                    let frame: TraceFrame =
                        serde_binary::from_slice(&data, binary_stream::Endian::Little)?;
                    return Ok((Some(frame), current_offset));
                }
            }
        }
    }

    fn check_counter(&mut self, counters: &[CounterSnapshot]) -> Vec<TraceFrame> {
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
        let buff: Vec<u8> = serde_binary::to_vec(&frame, binary_stream::Endian::Little)?;

        // First write length
        let len: u64 = buff.len() as u64;
        let len = len.to_le_bytes();

        let endoff = fd.stream_position()?;
        fd.write_all_at(&len, endoff)?;

        // And then write buff
        let endoff = fd.stream_position()?;
        fd.write_at(&buff, endoff)?;

        Ok(())
    }

    fn write_frame(&mut self, frame: TraceFrame) -> Result<(), Box<dyn Error>> {
        let mut fd = self.open(false)?;

        Self::do_write_frame(&mut fd, frame)?;

        self.size = fd.metadata()?.len();

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

    fn fold(&mut self) -> Result<(), Box<dyn Error>> {
        let (desc, data) = self.read_all()?;

        let meta: Vec<TraceFrame> = data.iter().filter(|v| v.is_metadata()).cloned().collect();

        let counters: Vec<TraceFrame> = data.iter().filter(|v| v.is_counters()).cloned().collect();

        // We have extracted all from the trace
        // We preemptively drop to save some mem
        drop(data);

        let mut newcounters: Vec<TraceFrame> = Vec::new();

        /* We skip the first record to keep TS 0 */
        let mut i = 1;
        while i + 1 < counters.len() {
            let sa = counters[i].clone();
            let sb = counters[i + 1].clone();

            // Now we only iterate on counters
            newcounters.push(sa.sum(sb)?);

            i += 2;
        }

        /* Now rewrite it all */
        remove_file(&self.path)?;

        /* Just recreate the file */
        let fd = self.open(true);
        drop(fd);

        /* Desc first */
        self.write_frame(desc)?;

        /* Then all metadata */
        for v in meta {
            self.write_frame(v)?;
        }

        /* And counters */
        for v in newcounters {
            self.write_frame(v)?;
        }

        Ok(())
    }

    fn push(&mut self, counters: Vec<CounterSnapshot>) -> Result<bool, Box<dyn Error>> {
        let new_counters: Vec<TraceFrame> = self.check_counter(&counters);

        self.write_frames(new_counters)?;

        /* Generate all counters */
        let counters: Vec<TraceCounter> = counters
            .iter()
            .map(|v| TraceCounter {
                id: self.counter_id(v).unwrap(),
                value: v.ctype.clone(),
            })
            .collect();

        /* Filter with respect to previous value */
        let counters = counters
            .iter()
            .map(|v| {
                self.last_value.insert(v.id, v.value.clone());
                v
            })
            .cloned()
            .collect();

        let frame = TraceFrame::Counters {
            ts: unix_ts(),
            counters,
        };

        self.write_frame(frame)?;

        if self.size as usize > self.max_size {
            self.fold()?;
            return Ok(true);
        }

        Ok(false)
    }

    fn read_all(&mut self) -> Result<(TraceFrame, Vec<TraceFrame>), Box<dyn Error>> {
        let mut frames = Vec::new();

        let mut fd = self.open(false)?;

        /* First frame is the desc */
        let (mut frame, mut current_offset) = Self::read_frame_at(&mut fd, 0)?;

        if frame.is_none() {
            return Err(ProxyErr::newboxed("Failed to read first frame"));
        }

        let desc = frame.unwrap();

        loop {
            (frame, current_offset) = Self::read_frame_at(&mut fd, current_offset)?;

            if frame.is_none() {
                return Ok((desc, frames));
            }

            /* Full frame */
            frames.push(frame.unwrap());
        }
    }

    fn new(path: &Path, job: &JobDesc, max_size: usize) -> Result<TraceState, Box<dyn Error>> {
        let ret = TraceState {
            size: 0,
            max_size,
            lastwrite: 0,
            path: path.to_path_buf(),
            counters: HashMap::new(),
            last_value: HashMap::new(),
            current_counter_id: 0,
        };

        let mut fd = ret.open(true)?;

        // First thing save the jobdesc
        let desc = TraceFrame::Desc {
            ts: unix_ts(),
            desc: job.clone(),
        };

        TraceState::do_write_frame(&mut fd, desc)?;

        Ok(ret)
    }

    fn from(path: &Path, max_size: usize) -> Result<TraceState, Box<dyn Error>> {
        let mut ret = TraceState {
            size: 0,
            max_size,
            lastwrite: 0,
            path: path.to_path_buf(),
            counters: HashMap::new(),
            last_value: HashMap::new(),
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
        let mut state = TraceState::from(path, 0)?;
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

    fn new(prefix: &Path, desc: &JobDesc, max_size: usize) -> Result<Trace, Box<dyn Error>> {
        let path = Trace::name(prefix, desc);
        if path.exists() {
            return Err(ProxyErr::newboxed(format!(
                "Cannot create trace it already exists at {}",
                path.to_string_lossy(),
            )));
        }

        let state = TraceState::new(&path, desc, max_size)?;

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

    pub(crate) fn push(
        &self,
        profile: JobProfile,
        current_sampling: u64,
    ) -> Result<Option<u64>, Box<dyn Error>> {
        let done = self.done.read().unwrap();

        if *done {
            return Err(ProxyErr::newboxed("Job is done"));
        }

        let sampling = if self.state.lock().unwrap().push(profile.counters)? {
            Some(current_sampling * 2)
        } else {
            None
        };

        Ok(sampling)
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

    pub(crate) fn metrics(&self, jobid: String) -> Result<Vec<String>, ProxyErr> {
        let metrics = self.read(jobid, None)?;

        let metrics: Vec<String> = metrics
            .frames
            .iter()
            .filter(|v| v.is_metadata())
            .map(|v| match v {
                TraceFrame::CounterMetadata { ts: _, metadata } => metadata.name.to_string(),
                _ => unreachable!(),
            })
            .collect();

        Ok(metrics)
    }

    pub(crate) fn read(
        &self,
        jobid: String,
        filter: Option<String>,
    ) -> Result<TraceRead, ProxyErr> {
        let ht = self.traces.read().unwrap();

        if let Some(trace) = ht.get(&jobid) {
            let (_, frames) = trace.state.lock().unwrap().read_all()?;

            let frames = if let Some(filter) = filter {
                let mut tmp_frames: Vec<TraceFrame> = Vec::new();
                let mut filter_id: Option<u64> = None;

                for f in frames.iter() {
                    match f {
                        TraceFrame::CounterMetadata { ts: _, metadata } => {
                            if metadata.name == filter {
                                tmp_frames.push(f.clone());
                                filter_id = Some(metadata.id);
                            }
                        }
                        TraceFrame::Desc { ts: _, desc: _ } => {}
                        TraceFrame::Counters { ts, counters } => {
                            if let Some(id) = filter_id {
                                let counters: Vec<TraceCounter> =
                                    counters.iter().filter(|v| v.id == id).cloned().collect();
                                if !counters.is_empty() {
                                    let counterframe = TraceFrame::Counters { ts: *ts, counters };
                                    tmp_frames.push(counterframe);
                                }
                            }
                        }
                    }
                }

                tmp_frames
            } else {
                frames
            };

            return Ok(TraceRead {
                info: TraceInfo::new(trace),
                frames,
            });
        }

        Err(ProxyErr::new(format!("No such trace id {}", jobid)))
    }

    pub(crate) fn plot(&self, jobid: String, filter: String) -> Result<Vec<(u64, f64)>, ProxyErr> {
        let trace = self.read(jobid, Some(filter))?;

        let mut ret: Vec<(u64, f64)> = Vec::new();

        for f in trace.frames.iter() {
            if let TraceFrame::Counters { ts, counters } = f {
                for c in counters {
                    match c.value {
                        CounterType::Counter { value } => {
                            ret.push((*ts, value));
                        }
                        CounterType::Gauge {
                            min: _,
                            max: _,
                            hits,
                            total,
                        } => {
                            ret.push((*ts, total / hits));
                        }
                    }
                }
            }
        }

        Ok(ret)
    }

    pub(crate) fn get(
        &self,
        jobdesc: &JobDesc,
        max_size: usize,
    ) -> Result<Arc<Trace>, Box<dyn Error>> {
        let mut ht = self.traces.write().unwrap();

        let trace = match ht.get(&jobdesc.jobid) {
            Some(v) => v.clone(),
            None => {
                let trace = Trace::new(&self.prefix, jobdesc, max_size)?;
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

        //self.traces.write().unwrap().remove(&job.jobid);
        Ok(())
    }

    pub(crate) fn new(prefix: &PathBuf) -> Result<TraceView, Box<dyn Error>> {
        let prefix = check_prefix_dir(prefix, "traces")?;
        let traces = RwLock::new(Self::load_existing_traces(&prefix)?);
        Ok(TraceView { prefix, traces })
    }
}
