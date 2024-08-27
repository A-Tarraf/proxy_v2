use crate::exporter::Exporter;
use crate::proxy_common::{self, is_url_live};
use crate::proxy_common::{unix_ts_us, ProxyErr};
use crate::proxywireprotocol::{self, CounterSnapshot, CounterType, JobDesc, JobProfile};
use crate::trace::{Trace, TraceView};
use crate::ExporterFactory;
use core::fmt;
use reqwest::blocking::Client;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::write;
use std::sync::Arc;
use std::vec;

use crate::systemmetrics::SystemMetrics;

enum ScraperType {
    Proxy,
    Prometheus,
    SystemMetrics {
        sys: Box<SystemMetrics>,
    },
    Trace {
        exporter: Arc<Exporter>,
        trace: Arc<Trace>,
    },
    Ftio {
        traces: Arc<TraceView>,
        jobid: String,
    },
}

impl fmt::Display for ScraperType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ScraperType::Proxy => write!(f, "Proxy"),
            ScraperType::Prometheus => write!(f, "Prometheus"),
            ScraperType::SystemMetrics { .. } => write!(f, "System"),
            ScraperType::Trace { exporter: _, trace } => {
                write!(f, "Trace job {} in {}", trace.desc().jobid, trace.path())
            }
            ScraperType::Ftio {
                traces: _,
                jobid: _,
            } => write!(f, "FTIO"),
        }
    }
}
pub struct ProxyScraper {
    target_url: String,
    state: HashMap<String, JobProfile>,
    factory: Option<Arc<ExporterFactory>>,
    period: f64,
    last_scrape: u128,
    ttype: ScraperType,
}

#[derive(Serialize)]
pub struct ProxyScraperSnapshot {
    target_url: String,
    ttype: String,
    period: f64,
    last_scrape: u64,
}

impl ProxyScraper {
    fn detect_type(target_url: &String) -> Result<(String, ScraperType), ProxyErr> {
        if target_url == "/system" {
            return Ok((
                target_url.to_string(),
                ScraperType::SystemMetrics {
                    sys: Box::new(SystemMetrics::new()),
                },
            ));
        }

        let url: String = if !target_url.starts_with("http") {
            "http://".to_string() + target_url.as_str()
        } else {
            target_url.to_string()
        };

        /* Now determine the type first as a Proxy Exporter */
        let test_page_url = url.clone() + "/is_admire_proxy.html";
        if is_url_live(&test_page_url, true).is_ok() {
            log::info!("{} is a Proxy Exporter", url);
            let joburl = url.clone() + "/job";
            return Ok((joburl, ScraperType::Proxy));
        }

        /* First as a prometheus exporter */
        let promurl = url.to_string() + "/metrics";
        if is_url_live(&promurl, false).is_ok() {
            log::info!("{} is a Prometheus Exporter", url);
            return Ok((promurl, ScraperType::Prometheus));
        }

        Err(ProxyErr::new(
            format!("Failed to determine type of {}", target_url).as_str(),
        ))
    }

    pub(crate) fn new(
        target_url: &String,
        period: f64,
        factory: Arc<ExporterFactory>,
    ) -> Result<ProxyScraper, ProxyErr> {
        let (url, ttype) = ProxyScraper::detect_type(target_url)?;
        log::info!("Creating a scrapper to {} for a period of {}", url, period);
        Ok(ProxyScraper {
            target_url: url,
            state: HashMap::new(),
            factory: Some(factory),
            period,
            last_scrape: 0,
            ttype,
        })
    }

    pub(crate) fn newtrace(
        exporter: Arc<Exporter>,
        trace: Arc<Trace>,
    ) -> Result<ProxyScraper, ProxyErr> {
        Ok(ProxyScraper {
            target_url: format!("/trace.{}", trace.desc().jobid),
            state: HashMap::new(),
            factory: None,
            period: 0.5,
            last_scrape: 0,
            ttype: ScraperType::Trace { exporter, trace },
        })
    }

    pub(crate) fn newftio(
        traces: Arc<TraceView>,
        jobid: &String,
    ) -> Result<ProxyScraper, ProxyErr> {
        Ok(ProxyScraper {
            target_url: format!("/FTIO/{}", jobid),
            state: HashMap::new(),
            factory: None,
            period: 30.0,
            last_scrape: 0,
            ttype: ScraperType::Ftio {
                traces,
                jobid: jobid.to_string(),
            },
        })
    }

    #[allow(unused)]
    pub(crate) fn snapshot(&self) -> ProxyScraperSnapshot {
        ProxyScraperSnapshot {
            target_url: self.target_url.to_string(),
            ttype: self.ttype.to_string(),
            period: self.period,
            last_scrape: (self.last_scrape / 1000000) as u64,
        }
    }

    pub(crate) fn url(&self) -> &String {
        &self.target_url
    }

    fn scrape_proxy(&mut self) -> Result<(), Box<dyn Error>> {
        let mut deleted: Vec<JobDesc> = Vec::new();

        let client = Client::new();
        let response = client.get(&self.target_url).send()?;

        // Check if the response was successful (status code 200 OK)
        if response.status().is_success() {
            // Deserialize the JSON response into your data structure
            let mut profiles: Vec<JobProfile> = response.json()?;
            let new_keys: HashSet<String> = profiles.iter().map(|v| v.desc.jobid.clone()).collect();

            /* First detect if a job has left */
            for (k, v) in self.state.iter() {
                if !new_keys.contains(k) {
                    /* Key has been dropped save name in list for notify */
                    deleted.push(v.desc.clone());
                    if let Some(factory) = &self.factory {
                        factory.relax_job(&v.desc)?;
                    } else {
                        unreachable!("Proxy scrapes should have a factory");
                    }
                }
            }

            /* Remove all deleted from the shadow state */
            for k in deleted.iter() {
                self.state.remove(&k.jobid);
            }

            /* Now Update Values */
            let factory = if let Some(factory) = &self.factory {
                factory
            } else {
                unreachable!("Proxy scrapes should have a factory");
            };

            for p in profiles.iter_mut() {
                log::trace!("Scraping {} from {}", p.desc.jobid, self.target_url);
                let cur: JobProfile;
                if let Some(previous) = self.state.get_mut(&p.desc.jobid) {
                    /* We clone previous snapshot before substracting */
                    cur = p.clone();
                    p.substract(previous)?;
                } else {
                    /* New Job Register in Job List */
                    let _ = factory.resolve_job(&p.desc, false);
                    cur = p.to_owned();
                }

                if let Some(exporter) = factory.resolve_by_id(&p.desc.jobid) {
                    for cnt in p.counters.iter() {
                        exporter.push(cnt)?;
                        exporter.accumulate(cnt, true)?;
                    }
                } else {
                    return Err(ProxyErr::newboxed("No such JobID"));
                }

                /* Now insert the non-substracted for next call state */
                self.state.insert(p.desc.jobid.to_string(), cur);
            }
        } else {
            return Err(ProxyErr::newboxed("Failed to make scraping request"));
        }

        Ok(())
    }

    fn prometheus_sample_name(s: &prometheus_parse::Sample) -> String {
        let mut name = s.metric.to_string();

        if !s.labels.is_empty() {
            name = format!("{}{{{}\"}}", name, s.labels);
        }

        name
    }

    fn scrape_prometheus(&mut self) -> Result<(), Box<dyn Error>> {
        let client = Client::new();
        let response = client.get(&self.target_url).send()?;
        let data = response.text()?;

        let lines: Vec<_> = data.lines().map(|s| Ok(s.to_string())).collect();
        let metrics = prometheus_parse::Scrape::parse(lines.into_iter())?;

        let factory = if let Some(factory) = &self.factory {
            factory
        } else {
            unreachable!("Proxy scrapes should have a factory");
        };

        // We push in MAIN, NODE and All exporters which may generate profiles
        // THese exporters are the one attached locally and thus bound to
        // node local performance
        let mut target_exporters: Vec<Arc<Exporter>> = vec![factory.get_main(), factory.get_node()];

        if let Ok(mut locals) = factory.get_local_job_exporters() {
            target_exporters.append(&mut locals);

            for v in metrics.samples {
                let doc: String = metrics
                    .docs
                    .get(&v.metric)
                    .unwrap_or(&"".to_string())
                    .clone();

                let entry: Option<CounterSnapshot> = match v.value {
                    prometheus_parse::Value::Counter(value) => Some(CounterSnapshot {
                        name: ProxyScraper::prometheus_sample_name(&v),
                        ctype: CounterType::Counter {
                            ts: proxy_common::unix_ts(),
                            value,
                        },
                        doc,
                    }),
                    prometheus_parse::Value::Gauge(value) => Some(CounterSnapshot {
                        name: ProxyScraper::prometheus_sample_name(&v),
                        ctype: CounterType::Gauge {
                            min: 0.0,
                            max: 0.0,
                            hits: 1.0,
                            total: value,
                        },
                        doc,
                    }),
                    _ => None,
                };

                for e in target_exporters.iter() {
                    if let Some(m) = entry.clone() {
                        e.push(&m)?;
                        e.accumulate(&m, false)?;
                    }
                }
            }
        }

        Ok(())
    }

    fn scrape_system_metrics(&mut self) -> Result<(), Box<dyn Error>> {
        let sys = match &mut self.ttype {
            ScraperType::SystemMetrics { sys } => sys,
            _ => {
                unreachable!();
            }
        };

        let factory = if let Some(factory) = &self.factory {
            factory
        } else {
            unreachable!("Proxy scrapes should have a factory");
        };

        let metrics = sys.scrape()?;

        // We push in MAIN, NODE and All exporters which may generate profiles
        // THese exporters are the one attached locally and thus bound to
        // node local performance
        let mut target_exporters: Vec<Arc<Exporter>> = vec![factory.get_main(), factory.get_node()];

        if let Ok(mut locals) = factory.get_local_job_exporters() {
            target_exporters.append(&mut locals);

            for e in target_exporters {
                for m in metrics.iter() {
                    e.push(m)?;
                    e.accumulate(m, false)?;
                }
            }
        }

        Ok(())
    }

    fn scrape_trace(
        &mut self,
        exporter: Arc<Exporter>,
        trace: Arc<Trace>,
    ) -> Result<(), Box<dyn Error>> {
        let data = exporter.profile(trace.desc(), false)?;
        if let Some(new_sampling) = trace.push(data, self.period)? {
            log::info!(
                "Lowering sampling period for {} to {} s",
                trace.path(),
                new_sampling
            );
            self.period = new_sampling;
        }

        Ok(())
    }

    fn scrape_ftio(&mut self, traces: Arc<TraceView>, jobid: String) -> Result<(), Box<dyn Error>> {
        traces.generate_ftio_model(&jobid)?;
        Ok(())
    }

    pub(crate) fn scrape(&mut self) -> Result<(), Box<dyn Error>> {
        if unix_ts_us() - self.last_scrape < (self.period * 1000000.0) as u128 {
            /* Not to be scraped yet */
            return Ok(());
        }

        log::debug!("Scraping {}", self.target_url);

        match &self.ttype {
            ScraperType::Proxy => {
                self.scrape_proxy()?;
            }
            ScraperType::Prometheus => {
                self.scrape_prometheus()?;
            }
            ScraperType::SystemMetrics { .. } => {
                self.scrape_system_metrics()?;
            }
            ScraperType::Trace { exporter, trace } => {
                self.scrape_trace(exporter.clone(), trace.clone())?;
            }
            ScraperType::Ftio { traces, jobid } => {
                self.scrape_ftio(traces.clone(), jobid.clone())?;
            }
        }

        self.last_scrape = unix_ts_us();

        Ok(())
    }
}
