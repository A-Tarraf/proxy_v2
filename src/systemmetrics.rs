use sysinfo::{ComponentExt, CpuExt, DiskExt, NetworkExt, System, SystemExt};

use crate::{
    proxy_common::{unix_ts, ProxyErr},
    proxywireprotocol::{CounterSnapshot, CounterType},
};

pub struct SystemMetrics {
    sys: System,
    last_scrape: u64,
}

impl SystemMetrics {
    pub fn new() -> SystemMetrics {
        SystemMetrics {
            sys: System::new_all(),
            last_scrape: unix_ts(),
        }
    }

    fn scrape_disks(&self, counters: &mut Vec<CounterSnapshot>) -> Result<(), ProxyErr> {
        for d in self.sys.disks() {
            let attrs: Vec<(String, String)> = vec![
                ("kind".to_string(), format!("{:?}", d.kind())),
                ("device".to_string(), d.name().to_string_lossy().to_string()),
                (
                    "fs".to_string(),
                    String::from_utf8(d.file_system().to_vec()).unwrap_or("unknown".to_string()),
                ),
                (
                    "mountpoint".to_string(),
                    d.mount_point().to_string_lossy().to_string(),
                ),
            ];

            let total_space: f64 = d.total_space() as f64;
            counters.push(CounterSnapshot::new(
                "proxy_disk_size_bytes".to_string(),
                attrs.as_slice(),
                "Total size in bytes of the given device".to_string(),
                CounterType::Gauge {
                    min: 0.0,
                    max: total_space,
                    hits: 1.0,
                    total: total_space,
                },
            ));

            let free_space = d.available_space() as f64;
            counters.push(CounterSnapshot::new(
                "proxy_disk_free_size_bytes".to_string(),
                attrs.as_slice(),
                "Total remaining size in bytes of the given device".to_string(),
                CounterType::Gauge {
                    min: 0.0,
                    max: free_space,
                    hits: 1.0,
                    total: free_space,
                },
            ));

            let used_space = total_space - free_space;
            counters.push(CounterSnapshot::new(
                "proxy_disk_used_size_bytes".to_string(),
                attrs.as_slice(),
                "Total used size in bytes of the given device".to_string(),
                CounterType::Gauge {
                    min: 0.0,
                    max: used_space,
                    hits: 1.0,
                    total: used_space,
                },
            ));

            let disk_usage = (used_space * 100.0) / total_space;
            counters.push(CounterSnapshot::new(
                "proxy_disk_usage_percent".to_string(),
                attrs.as_slice(),
                "Total used percentage of the given device".to_string(),
                CounterType::Gauge {
                    min: 0.0,
                    max: disk_usage,
                    hits: 1.0,
                    total: disk_usage,
                },
            ));
        }

        Ok(())
    }

    fn scrape_network_cards(&self, counters: &mut Vec<CounterSnapshot>) -> Result<(), ProxyErr> {
        let now = unix_ts();

        for (interface_name, data) in self.sys.networks() {
            let attrs: Vec<(String, String)> =
                vec![("interface".to_string(), interface_name.to_string())];

            if now != self.last_scrape {
                let transmitted = (data.transmitted() / (now - self.last_scrape)) as f64;
                counters.push(CounterSnapshot::new(
                    "proxy_network_transmit_bandwidth_bytes".to_string(),
                    attrs.as_slice(),
                    "Outgoing Bandwidth during the refresh interval on the given device"
                        .to_string(),
                    CounterType::Gauge {
                        min: 0.0,
                        max: transmitted,
                        hits: 1.0,
                        total: transmitted,
                    },
                ));

                let received = (data.received() / (now - self.last_scrape)) as f64;
                counters.push(CounterSnapshot::new(
                    "proxy_network_receive_bandwidth_bytes".to_string(),
                    attrs.as_slice(),
                    "Incoming Bandwidth during the refresh interval on the given device"
                        .to_string(),
                    CounterType::Gauge {
                        min: 0.0,
                        max: received,
                        hits: 1.0,
                        total: received,
                    },
                ));

                let transmitted = (data.packets_transmitted() / (now - self.last_scrape)) as f64;
                counters.push(CounterSnapshot::new(
                    "proxy_network_transmit_packet_rate".to_string(),
                    attrs.as_slice(),
                    "Outgoing packet rate during the refresh interval on the given device"
                        .to_string(),
                    CounterType::Gauge {
                        min: 0.0,
                        max: transmitted,
                        hits: 1.0,
                        total: transmitted,
                    },
                ));

                let received = (data.packets_received() / (now - self.last_scrape)) as f64;
                counters.push(CounterSnapshot::new(
                    "proxy_network_receive_packet_rate".to_string(),
                    attrs.as_slice(),
                    "Incoming packet rate during the refresh interval on the given device"
                        .to_string(),
                    CounterType::Gauge {
                        min: 0.0,
                        max: received,
                        hits: 1.0,
                        total: received,
                    },
                ));
            }

            let transmitted = data.total_transmitted() as f64;
            counters.push(CounterSnapshot::new(
                "proxy_network_transmit_bytes_total".to_string(),
                attrs.as_slice(),
                "Total number of bytes sent on the given device".to_string(),
                CounterType::Counter { value: transmitted },
            ));

            let received = data.total_received() as f64;
            counters.push(CounterSnapshot::new(
                "proxy_network_receive_bytes_total".to_string(),
                attrs.as_slice(),
                "Total number of bytes received on the given device".to_string(),
                CounterType::Counter { value: received },
            ));

            let transmitted = data.total_packets_transmitted() as f64;
            counters.push(CounterSnapshot::new(
                "proxy_network_transmit_packets_total".to_string(),
                attrs.as_slice(),
                "Total number of packets sent on the given device".to_string(),
                CounterType::Counter { value: transmitted },
            ));

            let received = data.total_packets_received() as f64;
            counters.push(CounterSnapshot::new(
                "proxy_network_receive_packets_total".to_string(),
                attrs.as_slice(),
                "Total number of packets received on the given device".to_string(),
                CounterType::Counter { value: received },
            ));

            let transmitted = data.total_errors_on_transmitted() as f64;
            counters.push(CounterSnapshot::new(
                "proxy_network_transmit_packets_error_total".to_string(),
                attrs.as_slice(),
                "Total number of erroneous packets sent on the given device".to_string(),
                CounterType::Counter { value: transmitted },
            ));

            let received = data.total_errors_on_received() as f64;
            counters.push(CounterSnapshot::new(
                "proxy_network_receive_packets_error_total".to_string(),
                attrs.as_slice(),
                "Total number of erroneous  packets received on the given device".to_string(),
                CounterType::Counter { value: received },
            ));
        }

        Ok(())
    }

    fn scrape_temperatures(&self, counters: &mut Vec<CounterSnapshot>) -> Result<(), ProxyErr> {
        for c in self.sys.components() {
            let attrs: Vec<(String, String)> =
                vec![("component".to_string(), c.label().to_string())];

            let curtemp = c.temperature() as f64;
            counters.push(CounterSnapshot::new(
                "proxy_component_temperature_celcius".to_string(),
                attrs.as_slice(),
                "Current temperature in celcius for the given component".to_string(),
                CounterType::Gauge {
                    min: 0.0,
                    max: curtemp,
                    hits: 1.0,
                    total: curtemp,
                },
            ));

            /*
                       let temp = c.max() as f64;
                       counters.push(CounterSnapshot::new(
                           "proxy_component_max_seen_temperature_celcius".to_string(),
                           attrs.as_slice(),
                           "Maximum temperature seen in the past in celcius for the given component"
                               .to_string(),
                           CounterType::Gauge {
                               min: 0.0,
                               max: temp,
                               hits: 1.0,
                               total: temp,
                           },
                       ));
            */
            if let Some(temp) = c.critical() {
                counters.push(CounterSnapshot::new(
                    "proxy_component_critical_temperature_celcius".to_string(),
                    attrs.as_slice(),
                    "Critical temperature in celcius for the given component".to_string(),
                    CounterType::Gauge {
                        min: 0.0,
                        max: temp as f64,
                        hits: 1.0,
                        total: temp as f64,
                    },
                ));

                let crit = if curtemp >= temp as f64 { 1 } else { 0 };

                counters.push(CounterSnapshot::new(
                    "proxy_component_critical_temperature".to_string(),
                    attrs.as_slice(),
                    "A boolean indicating if the component reached a critical temperature"
                        .to_string(),
                    CounterType::Gauge {
                        min: 0.0,
                        max: crit as f64,
                        hits: 1.0,
                        total: crit as f64,
                    },
                ));
            }
        }

        Ok(())
    }

    fn scrape_memory(&self, counters: &mut Vec<CounterSnapshot>) -> Result<(), ProxyErr> {
        let total_mem = self.sys.total_memory() as f64;
        counters.push(CounterSnapshot::new(
            "proxy_memory_total_bytes".to_string(),
            &[],
            "Total memory on the system in bytes".to_string(),
            CounterType::Gauge {
                min: 0.0,
                max: total_mem,
                hits: 1.0,
                total: total_mem,
            },
        ));

        let used_mem = self.sys.used_memory() as f64;
        counters.push(CounterSnapshot::new(
            "proxy_memory_used_bytes".to_string(),
            &[],
            "Total memory on the system in bytes".to_string(),
            CounterType::Gauge {
                min: 0.0,
                max: used_mem,
                hits: 1.0,
                total: used_mem,
            },
        ));

        let usedpct = used_mem * 100.0 / total_mem;
        counters.push(CounterSnapshot::new(
            "proxy_memory_used_percent".to_string(),
            &[],
            "Total memory usage on the system in percent".to_string(),
            CounterType::Gauge {
                min: 0.0,
                max: usedpct,
                hits: 1.0,
                total: usedpct,
            },
        ));

        let total_swp = self.sys.total_memory() as f64;
        counters.push(CounterSnapshot::new(
            "proxy_swap_total_bytes".to_string(),
            &[],
            "Total swap size on the system in bytes".to_string(),
            CounterType::Gauge {
                min: 0.0,
                max: total_swp,
                hits: 1.0,
                total: total_swp,
            },
        ));

        let used_swp = self.sys.used_swap() as f64;
        counters.push(CounterSnapshot::new(
            "proxy_swap_used_bytes".to_string(),
            &[],
            "Total used swap on the system in bytes".to_string(),
            CounterType::Gauge {
                min: 0.0,
                max: used_swp,
                hits: 1.0,
                total: used_swp,
            },
        ));

        let usedpct = used_swp * 100.0 / total_swp;
        counters.push(CounterSnapshot::new(
            "proxy_memory_swap_used_percent".to_string(),
            &[],
            "Total swap usage on the system in percent".to_string(),
            CounterType::Gauge {
                min: 0.0,
                max: usedpct,
                hits: 1.0,
                total: usedpct,
            },
        ));

        Ok(())
    }

    fn scrape_system_info(&self, counters: &mut Vec<CounterSnapshot>) -> Result<(), ProxyErr> {
        let attrs: Vec<(String, String)> = vec![
            (
                "os".to_string(),
                self.sys.name().unwrap_or("unknown".to_string()).to_string(),
            ),
            (
                "osversion".to_string(),
                self.sys
                    .os_version()
                    .unwrap_or("unknown".to_string())
                    .to_string(),
            ),
            (
                "kernel".to_string(),
                self.sys
                    .kernel_version()
                    .unwrap_or("unknown".to_string())
                    .to_string(),
            ),
            (
                "hostname".to_string(),
                self.sys
                    .host_name()
                    .unwrap_or("unknown".to_string())
                    .to_string(),
            ),
        ];
        counters.push(CounterSnapshot::new(
            "proxy_scrape_total".to_string(),
            attrs.as_slice(),
            "Number of scrapes for proxy instance".to_string(),
            CounterType::Counter { value: 1.0 },
        ));
        Ok(())
    }

    fn scrape_cpu(&self, counters: &mut Vec<CounterSnapshot>) -> Result<(), ProxyErr> {
        let cpus = self.sys.cpus();

        let mut total_load: f64 = 0.0;

        for c in cpus {
            let attrs: Vec<(String, String)> = vec![
                ("name".to_string(), c.name().to_string()),
                ("vendor".to_string(), c.vendor_id().to_string()),
                ("model".to_string(), c.brand().to_string()),
            ];
            let freq = c.frequency() as f64;
            counters.push(CounterSnapshot::new(
                "proxy_cpu_frequency_ghz".to_string(),
                attrs.as_slice(),
                "Current frequency of the given CPU".to_string(),
                CounterType::Gauge {
                    min: 0.0,
                    max: freq,
                    hits: 1.0,
                    total: freq,
                },
            ));
            let load = c.cpu_usage() as f64;
            total_load += load;
            counters.push(CounterSnapshot::new(
                "proxy_cpu_usage_percent".to_string(),
                attrs.as_slice(),
                "Current load in percent of the given CPU".to_string(),
                CounterType::Gauge {
                    min: 0.0,
                    max: load,
                    hits: 1.0,
                    total: load,
                },
            ));
        }
        let cpucnt: f64 = cpus.len() as f64;
        counters.push(CounterSnapshot::new(
            "proxy_cpu_total".to_string(),
            &[],
            "Number of tracked CPUs by individual proxies".to_string(),
            CounterType::Gauge {
                min: 0.0,
                max: cpucnt,
                hits: 1.0,
                total: cpucnt,
            },
        ));

        let avg_load: f64 = total_load / cpucnt;
        counters.push(CounterSnapshot::new(
            "proxy_cpu_load_average_percent".to_string(),
            &[],
            "Average load on all the CPUs".to_string(),
            CounterType::Gauge {
                min: 0.0,
                max: avg_load,
                hits: 1.0,
                total: avg_load,
            },
        ));
        Ok(())
    }

    pub(crate) fn scrape(&mut self) -> Result<Vec<CounterSnapshot>, ProxyErr> {
        let mut ret: Vec<CounterSnapshot> = Vec::new();

        self.sys.refresh_disks_list();
        self.sys.refresh_disks();
        self.scrape_disks(&mut ret)?;

        self.sys.refresh_networks_list();
        self.sys.refresh_networks();
        self.scrape_network_cards(&mut ret)?;

        self.sys.refresh_components_list();
        self.sys.refresh_components();
        self.scrape_temperatures(&mut ret)?;

        self.sys.refresh_memory();
        self.scrape_memory(&mut ret)?;

        self.scrape_system_info(&mut ret)?;

        self.sys.refresh_cpu();
        self.scrape_cpu(&mut ret)?;

        /* Flag the last scrape TS */
        self.last_scrape = unix_ts();

        Ok(ret)
    }
}
