use std::io::{self, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::{
    collections::HashMap,
    error::Error,
    fs::{remove_file, File, OpenOptions},
    io::Seek,
    os::unix::prelude::FileExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use rayon::{
    iter::{IntoParallelRefIterator, ParallelIterator},
    slice::ParallelSlice,
};
use serde::{Deserialize, Serialize};
use serde_binary::binary_stream;

use crate::{
    exporter::ExporterFactory,
    proxy_common::{check_prefix_dir, list_files_with_ext_in, unix_ts, unix_ts_us, ProxyErr},
    proxywireprotocol::{max_f64, min_f64, CounterSnapshot, CounterType, JobDesc, JobProfile},
};

use crate::proxy_common::derivate_time_serie;
use crate::proxy_common::offset_time_serie;

/**********************
 * JSON TRACE SUPPORT *
 **********************/
#[derive(Serialize)]
pub struct TraceExport {
    pub infos: TraceInfo,
    pub metrics: HashMap<String, Vec<(f64, f64)>>,
}

impl TraceExport {
    pub fn new(infos: TraceInfo, traces: &TraceView) -> Result<TraceExport, Box<dyn Error>> {
        let mut ret = TraceExport {
            infos,
            metrics: HashMap::new(),
        };

        ret.load(traces)?;

        Ok(ret)
    }

    pub fn set(&mut self, name: String, values: Vec<(f64, f64)>) -> Result<(), ProxyErr> {
        self.metrics.insert(name, values);
        Ok(())
    }

    fn load(&mut self, traces: &TraceView) -> Result<(), Box<dyn Error>> {
        let metrics = traces.metrics(&self.infos.desc.jobid)?;
        let full_data = traces.full_read(&self.infos.desc.jobid)?;

        let mut offset: Option<f64> = None;

        /* Get the minimum timestamp on series */
        full_data.series.iter().for_each(|(_, counter_vec)| {
            if let Some((ts, _)) = counter_vec.first() {
                match offset {
                    Some(v) => {
                        if *ts < v {
                            offset = Some(*ts);
                        }
                    }
                    None => offset = Some(*ts),
                }
            }
        });

        let offset = offset.unwrap_or(0.0);

        // Define a type alias for the inner tuple
        type MetricTuple = (f64, f64);

        // Define a type alias for the main vector
        type CollectedMetrics = Vec<(String, Vec<MetricTuple>, Vec<MetricTuple>)>;

        /* Now for all metrics we get the data and its derivate and we store in the output hashtable */
        let collected_metrics: CollectedMetrics = metrics
            .par_iter()
            .filter_map(|m| {
                let id = if let Some(m) = full_data.counters.get(m) {
                    m.id
                } else {
                    unreachable!();
                };

                let mut data = if let Some(d) = full_data.series.get(&id) {
                    TraceView::to_time_serie(d)
                } else {
                    return None;
                };

                /* Fix temporal offset */
                offset_time_serie(&mut data, offset);

                /* Derivate the data  */
                let deriv = derivate_time_serie(&data);

                Some((m.clone(), data, deriv))
            })
            .collect();

        for (m, data, deriv) in collected_metrics {
            self.set(m.clone(), data)?;
            self.set(format!("deriv__{}", m), deriv)?;
        }

        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct TraceHeader {
    desc: JobDesc,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct TraceCounter {
    id: u64,
    value: CounterType,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct TraceCounterMetadata {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) doc: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) enum TraceFrame {
    Desc {
        ts: f64,
        desc: JobDesc,
    },
    CounterMetadata {
        ts: f64,
        metadata: TraceCounterMetadata,
    },
    Counters {
        ts: f64,
        counters: Vec<TraceCounter>,
    },
}

impl TraceFrame {
    fn ts(&self) -> f64 {
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

    fn mergecounters(first: Vec<TraceCounter>, second: &Vec<TraceCounter>) -> Vec<TraceCounter> {
        let first = first.iter().map(|v| (v.id, v)).collect::<HashMap<_, _>>();

        let ret: Vec<TraceCounter> = second
            .par_iter()
            .map(|v| {
                let ret = if let Some(prev) = first.get(&v.id) {
                    match v.value {
                        CounterType::Counter { ts, value } => match prev.value {
                            CounterType::Counter { ts: _, value: _ } => TraceCounter {
                                id: v.id,
                                value: CounterType::Counter { ts, value },
                            },
                            CounterType::Gauge { .. } => unreachable!(),
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
                            } => TraceCounter {
                                id: v.id,
                                value: CounterType::Gauge {
                                    min: min_f64(min, min2),
                                    max: max_f64(max, max2),
                                    hits: hits + hits2,
                                    total: total + total2,
                                },
                            },
                            CounterType::Counter { .. } => unreachable!(),
                        },
                    }
                } else {
                    v.clone()
                };
                ret
            })
            .collect();

        ret
    }

    fn sum(self, other: &TraceFrame) -> Result<TraceFrame, ProxyErr> {
        match self {
            TraceFrame::Counters { ts, counters } => match other {
                TraceFrame::Counters {
                    ts: tsb,
                    counters: countersb,
                } => Ok(TraceFrame::Counters {
                    ts: (*tsb + ts) / 2.0,
                    counters: TraceFrame::mergecounters(counters, countersb),
                }),
                _ => unreachable!("This function must take a counter"),
            },
            _ => unreachable!("This function must take a counter"),
        }
    }

    fn is_counters(&self) -> bool {
        matches!(self, TraceFrame::Counters { .. })
    }

    #[allow(unused)]
    fn is_desc(&self) -> bool {
        matches!(self, TraceFrame::Desc { .. })
    }

    fn is_metadata(&self) -> bool {
        matches!(self, TraceFrame::CounterMetadata { .. })
    }
}

#[derive(Clone)]
pub(crate) struct TraceData {
    pub(crate) counters: HashMap<String, TraceCounterMetadata>,
    pub(crate) desc: TraceFrame,
    pub(crate) frames: Vec<TraceFrame>,
    pub(crate) series: HashMap<u64, Vec<(f64, CounterType)>>,
}

impl TraceData {
    fn clear(&mut self) {
        self.counters.clear();
        self.series.clear();
        self.frames = Vec::new();
    }

    fn push_counters(&mut self, ts: f64, counters: &Vec<TraceCounter>) {
        for c in counters {
            let counter_vec = self.series.entry(c.id).or_default();
            counter_vec.push((ts, c.value.clone()));
        }
    }

    fn append_data(&mut self, frames: &mut Vec<TraceFrame>) {
        for frame in frames.iter() {
            match frame {
                TraceFrame::Desc { ts: _, desc: _ } => {
                    self.desc = frame.clone();
                }
                TraceFrame::CounterMetadata { ts: _, metadata } => {
                    self.counters
                        .insert(metadata.name.clone(), metadata.clone());
                }
                TraceFrame::Counters { ts, counters } => {
                    self.push_counters(*ts, counters);
                }
            }
        }

        self.frames.append(frames);
    }

    #[allow(unused)]
    fn new(desc: TraceFrame, frames: &mut Vec<TraceFrame>) -> TraceData {
        let mut ret = TraceData::empty(&desc);
        ret.append_data(frames);
        ret
    }

    fn empty(desc: &TraceFrame) -> TraceData {
        TraceData {
            counters: HashMap::new(),
            desc: desc.clone(),
            frames: Vec::new(),
            series: HashMap::new(),
        }
    }

    fn push(&mut self, frame: TraceFrame) {
        let mut v = vec![frame];
        self.append_data(&mut v);
    }
}

/// This is the trace state main handle to a trace
/// when writing to it and when reading from it
/// The trace is read lazily only and the
/// TraceData if filled when read_all is called
#[allow(unused)]
struct TraceState {
    /// Was the trace loaded already ?
    loaded: bool,
    /// Current size of the trace
    size: u64,
    /// Maximum size of the trace
    max_size: usize,
    /// Timestamp of the last write to the trace
    lastwrite: f64,
    /// Path of the trace
    path: PathBuf,

    /// Current counter identifier of the trace
    current_counter_id: u64,

    /// Read state
    trace_data: TraceData,
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

    fn desc_from_file(path: &PathBuf) -> Result<JobDesc, Box<dyn Error>> {
        let mut fd = File::open(path)?;
        let (data, _) = Self::read_frame_at(&mut fd, 0)?;

        if let Some(frame) = data {
            return Ok(frame.desc()?);
        }

        Err(ProxyErr::newboxed(
            "First frame of the trace is not a trace description",
        ))
    }

    fn desc(&mut self) -> Result<JobDesc, Box<dyn Error>> {
        TraceState::desc_from_file(&self.path)
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
            if !self.trace_data.counters.contains_key(&c.name) {
                let metadata = TraceCounterMetadata {
                    id: self.current_counter_id,
                    name: c.name.to_string(),
                    doc: c.doc.to_string(),
                };

                self.current_counter_id += 1;

                self.trace_data
                    .counters
                    .insert(c.name.to_string(), metadata.clone());

                let frame = TraceFrame::CounterMetadata {
                    ts: unix_ts_us(),
                    metadata,
                };

                ret.push(frame)
            }
        }
        ret
    }

    fn counter_id(&self, counter: &CounterSnapshot) -> Option<u64> {
        if let Some(c) = self.trace_data.counters.get(&counter.name) {
            return Some(c.id);
        }

        None
    }

    fn do_write_frame(fd: &mut File, frame: &TraceFrame) -> Result<(), Box<dyn Error>> {
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

    fn write_frame(&mut self, frame: &TraceFrame) -> Result<(), Box<dyn Error>> {
        let mut fd = self.open(false)?;

        Self::do_write_frame(&mut fd, frame)?;

        self.size = fd.metadata()?.len();

        Ok(())
    }

    fn write_frames(&mut self, frames: &Vec<TraceFrame>) -> Result<(), Box<dyn Error>> {
        if frames.is_empty() {
            return Ok(());
        }

        let mut fd = self.open(false)?;

        for f in frames.iter() {
            Self::do_write_frame(&mut fd, f)?;
        }

        self.lastwrite = unix_ts_us();
        self.size = fd.metadata()?.len();

        Ok(())
    }

    fn fold(&mut self) -> Result<(), Box<dyn Error>> {
        let desc = self.trace_data.desc.clone();

        let mut meta: Vec<TraceFrame> = self
            .trace_data
            .frames
            .iter()
            .filter(|v| v.is_metadata())
            .cloned()
            .collect();

        let counters: Vec<TraceFrame> = self
            .trace_data
            .frames
            .iter()
            .filter(|v| v.is_counters())
            .cloned()
            .collect();

        let mut newcounters: Vec<TraceFrame> = counters
            .par_chunks(2)
            .flat_map(|chunk| {
                if chunk.len() == 2 {
                    let sa = chunk[0].clone();
                    sa.sum(&chunk[1]).ok()
                } else {
                    None
                }
            })
            .collect();

        /* Now rewrite it all */
        remove_file(&self.path)?;

        /* Just recreate the file */
        let fd = self.open(true);
        drop(fd);

        /* Desc first */
        self.write_frame(&desc)?;

        /* Then all metadata */
        for v in meta.iter() {
            self.write_frame(v)?;
        }

        /* And counters */
        for v in newcounters.iter() {
            self.write_frame(v)?;
        }

        /* Update in memory state */
        self.trace_data.clear();
        self.trace_data.append_data(&mut meta);
        self.trace_data.append_data(&mut newcounters);

        Ok(())
    }

    fn push(&mut self, counters: Vec<CounterSnapshot>) -> Result<bool, Box<dyn Error>> {
        let mut new_counters: Vec<TraceFrame> = self.check_counter(&counters);

        self.write_frames(&new_counters)?;
        self.trace_data.append_data(&mut new_counters);

        /* Generate all counters */
        let counters: Vec<TraceCounter> = counters
            .iter()
            .map(|v| TraceCounter {
                id: self.counter_id(v).unwrap(),
                value: v.ctype.clone(),
            })
            .collect();

        let ts = counters
            .first()
            .map(|v| v.value.ts())
            .unwrap_or(unix_ts_us());

        let frame = TraceFrame::Counters { ts, counters };

        /* Add to file */
        self.write_frame(&frame)?;
        /* Add to in-memory state */
        self.trace_data.push(frame);

        if self.size as usize > self.max_size {
            self.fold()?;
            return Ok(true);
        }

        Ok(false)
    }

    fn read_all(&mut self) -> Result<Vec<TraceFrame>, Box<dyn Error>> {
        let mut frames = Vec::new();

        let mut fd = self.open(false)?;

        /* First frame is the desc */
        let mut current_offset: u64 = 0;
        let mut frame: Option<TraceFrame>;

        loop {
            (frame, current_offset) = Self::read_frame_at(&mut fd, current_offset)?;

            if frame.is_none() {
                return Ok(frames);
            }

            /* Full frame */
            frames.push(frame.unwrap());
        }
    }

    fn new(path: &Path, job: &JobDesc, max_size: usize) -> Result<TraceState, Box<dyn Error>> {
        // First thing save the jobdesc
        let desc = TraceFrame::Desc {
            ts: unix_ts_us(),
            desc: job.clone(),
        };

        let ret = TraceState {
            loaded: true, // Trace is new thus already loaded
            size: 0,
            max_size,
            lastwrite: 0.0,
            path: path.to_path_buf(),
            current_counter_id: 0,
            trace_data: TraceData::empty(&desc),
        };

        let mut fd = ret.open(true)?;

        TraceState::do_write_frame(&mut fd, &desc)?;

        Ok(ret)
    }

    fn from(path: &Path, max_size: usize) -> Result<TraceState, Box<dyn Error>> {
        let desc = TraceState::desc_from_file(&path.to_path_buf())?;

        let desc = TraceFrame::Desc {
            ts: unix_ts_us(),
            desc: desc.clone(),
        };

        let mut ret = TraceState {
            loaded: false, // Trace is not loaded already
            size: 0,
            max_size,
            lastwrite: 0.0,
            path: path.to_path_buf(),
            current_counter_id: 0,
            trace_data: TraceData::empty(&desc),
        };

        let lastframe = ret.read_last()?;

        ret.size = ret.len()?;

        if let Some(f) = lastframe {
            ret.lastwrite = f.ts();
        }

        Ok(ret)
    }

    fn load(&mut self) -> Result<(), Box<dyn Error>> {
        if !self.loaded {
            let mut frames = self.read_all()?;
            self.trace_data.clear();
            self.trace_data.append_data(&mut frames);
            self.loaded = true;
        }
        Ok(())
    }

    fn metrics(&mut self) -> Result<Vec<String>, Box<dyn Error>> {
        self.load()?;
        Ok(self.trace_data.counters.keys().cloned().collect())
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
            desc.end_time = (state.lastwrite / 1000000) as u64;
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
        current_sampling: f64,
    ) -> Result<Option<f64>, Box<dyn Error>> {
        let done = self.done.read().unwrap();

        if *done {
            return Err(ProxyErr::newboxed("Job is done"));
        }

        let sampling = if self.state.lock().unwrap().push(profile.counters)? {
            Some(current_sampling * 2.0)
        } else {
            None
        };

        Ok(sampling)
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct TraceInfo {
    pub desc: JobDesc,
    pub size: u64,
    pub lastwrite: f64,
}

#[derive(Serialize)]
pub(crate) struct TraceRead {
    info: TraceInfo,
    time_serie: Vec<(f64, CounterType)>,
}

impl TraceInfo {
    pub(crate) fn new(trace: &Trace) -> TraceInfo {
        let infos = trace.state.lock().unwrap();
        TraceInfo {
            desc: trace.desc.clone(),
            size: infos.size,
            lastwrite: (infos.lastwrite / 1000),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct FtioModelTopFreq {
    freq: Vec<f64>,
    conf: Vec<f64>,
    amp: Vec<f64>,
    phi: Vec<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct FtioModel {
    metric: String,
    dominant_freq: Vec<f64>,
    conf: Vec<f64>,
    amp: Vec<f64>,
    phi: Vec<f64>,
    t_start: f64,
    t_end: f64,
    total_bytes: usize,
    ranks: usize,
    freq: usize,
    top_freq: FtioModelTopFreq,
}

#[derive(Serialize, Clone)]
pub struct FtioModelStorage {
    models: HashMap<String, FtioModel>,
}

impl FtioModelStorage {
    fn new() -> FtioModelStorage {
        FtioModelStorage {
            models: HashMap::new(),
        }
    }
}

pub(crate) struct TraceView {
    prefix: PathBuf,
    traces: RwLock<HashMap<String, Arc<Trace>>>,
    freq_models: RwLock<HashMap<String, FtioModelStorage>>,
}

impl TraceView {
    pub fn get_job_freq_model(&self, jobid: String) -> Option<FtioModelStorage> {
        let mut models: Option<FtioModelStorage> = None;

        if let Ok(freq_mods) = self.freq_models.read() {
            if let Some(job_metrics) = freq_mods.get(&jobid) {
                models = Some(job_metrics.clone());
            }
        }

        models
    }

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
        let path = Trace::name(&self.prefix, desc);
        if path.is_file() {
            log::error!("Removing {}", path.to_string_lossy());
            remove_file(path)?;
        }

        if let Ok(tr) = self.traces.write().as_mut() {
            if tr.contains_key(&desc.jobid) {
                tr.remove(&desc.jobid);
            }
        }

        Ok(())
    }

    #[allow(unused)]
    pub(crate) fn infos(&self, jobid: &String) -> Result<TraceInfo, ProxyErr> {
        let trace = self.read(jobid, None)?;
        Ok(trace.info)
    }

    pub(crate) fn metrics(&self, jobid: &String) -> Result<Vec<String>, ProxyErr> {
        let metrics = if let Some(tr) = self.traces.read().unwrap().get(jobid) {
            tr.state.lock().unwrap().metrics()?
        } else {
            return Err(ProxyErr::new("No such jobid"));
        };

        Ok(metrics)
    }

    #[allow(unused)]
    pub(crate) fn full_read(&self, jobid: &String) -> Result<TraceData, ProxyErr> {
        let ht = self.traces.read().unwrap();

        if let Some(trace) = ht.get(jobid) {
            if let Ok(mut locked_trace) = trace.state.lock() {
                locked_trace.load()?;
                return Ok(locked_trace.trace_data.clone());
            } else {
                unreachable!("Failed to acquire trace lock");
            }
        }

        Err(ProxyErr::new(format!("No such trace with jobid {}", jobid)))
    }

    pub(crate) fn read(
        &self,
        jobid: &String,
        metric_name: Option<String>,
    ) -> Result<TraceRead, ProxyErr> {
        let ht = self.traces.read().unwrap();

        if let Some(trace) = ht.get(jobid) {
            let time_serie = if let Ok(mut locked_trace) = trace.state.lock() {
                /* If we are here we need to read */
                locked_trace.load()?;

                let time_serie = if let Some(metric_name) = metric_name {
                    let time_serie = if let Some(metric) =
                        locked_trace.trace_data.counters.get(&metric_name)
                    {
                        let data =
                            if let Some(data) = locked_trace.trace_data.series.get(&metric.id) {
                                data
                            } else {
                                return Err(ProxyErr::new(format!(
                                    "Failed to retrieve metric data {}",
                                    metric_name
                                )));
                            };
                        data.clone()
                    } else {
                        return Err(ProxyErr::new(format!("No such metric {}", metric_name)));
                    };
                    time_serie
                } else {
                    let empty: Vec<(f64, CounterType)> = Vec::new();
                    empty
                };

                time_serie
            } else {
                unreachable!();
            };

            return Ok(TraceRead {
                info: TraceInfo::new(trace),
                time_serie,
            });
        }

        Err(ProxyErr::new(format!("No such trace id {}", jobid)))
    }

    pub(crate) fn to_time_serie(time_serie: &[(f64, CounterType)]) -> Vec<(f64, f64)> {
        let mut ret: Vec<(f64, f64)> = Vec::new();

        for (ts, c) in time_serie.iter() {
            match c {
                CounterType::Counter { ts: cnt_ts, value } => ret.push((*cnt_ts, *value)),
                CounterType::Gauge {
                    min: _,
                    max: _,
                    hits: _,
                    total: _,
                } => ret.push((*ts, c.value())),
            }
        }

        ret
    }

    #[allow(unused)]
    pub(crate) fn plot(&self, jobid: &String, filter: String) -> Result<Vec<(f64, f64)>, ProxyErr> {
        let trace = self.read(jobid, Some(filter))?;
        let ret = TraceView::to_time_serie(&trace.time_serie);
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

    pub(crate) fn export(&self, jobid: &String) -> Result<TraceExport, Box<dyn Error>> {
        TraceExport::new(self.infos(jobid)?, self)
    }

    pub(crate) fn generate_ftio_model(&self, jobid: &String) -> Result<(), Box<dyn Error>> {
        which::which("admire_proxy_invoke_ftio")?;

        let export = self.export(jobid)?;

        // Convert the Rust value into a string to be piped.
        let mut cmd = Command::new("admire_proxy_invoke_ftio")
            .arg("-n")
            .arg("10")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;

        let mut child_stdin = cmd.stdin.take().unwrap();

        // The subprocess outputs this string.
        child_stdin
            .write_all(serde_json::to_string(&export)?.as_bytes())
            .unwrap();
        drop(child_stdin);

        let output = cmd.wait_with_output()?;

        if output.status.success() {
            match serde_json::from_slice::<Vec<FtioModel>>(&output.stdout) {
                Ok(models) => {
                    if let Ok(job_model_ht) = self.freq_models.write().as_mut() {
                        let job_storage = job_model_ht
                            .entry(jobid.to_string())
                            .or_insert(FtioModelStorage::new());

                        for m in models {
                            log::debug!("FTIO Model for {}: {:?}", m.metric, m);
                            job_storage.models.insert(m.metric.to_string(), m);
                        }
                    }
                }
                Err(e) => {
                    log::error!("Failed to parse FTIO output: {}", e);
                }
            }
        }

        Ok(())
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
        let freq_models = RwLock::new(HashMap::new());
        Ok(TraceView {
            prefix,
            traces,
            freq_models,
        })
    }
}
