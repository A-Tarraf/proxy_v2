#[cfg(feature = "admire")]
mod admire {
    use crate::exporter::ExporterFactory;
    use crate::proxy_common::ProxyErr;
    use crate::proxywireprotocol::ValueAlarmTrigger;
    use rust_icc::*;
    use std::{
        collections::HashMap,
        ffi::CString,
        ptr,
        sync::Arc,
        thread::{self, JoinHandle},
        time::Duration,
    };

    pub struct IccClient {
        handle: *mut icc_context,
    }

    impl IccClient {
        pub unsafe fn new() -> Result<IccClient, ProxyErr> {
            let mut handle: *mut icc_context = ptr::null_mut();
            let ret = icc_init(
                icc_log_level_ICC_LOG_DEBUG,
                icc_client_type_ICC_TYPE_JOBMON,
                &mut handle,
            );

            if ret != icc_retcode_ICC_SUCCESS {
                return Err(ProxyErr::new(format!(
                    "Failed to connect to ICC rc {}",
                    ret
                )));
            }

            Ok(IccClient { handle })
        }

        pub unsafe fn close(&mut self) -> Result<(), ProxyErr> {
            let ret = icc_fini(self.handle);

            if ret != icc_retcode_ICC_SUCCESS {
                return Err(ProxyErr::new(format!(
                    "Failed to connect to ICC rc {}",
                    ret
                )));
            }

            Ok(())
        }

        pub unsafe fn test(&mut self, value: u8) -> Result<i32, ProxyErr> {
            let mut retcode = 0;
            let ret = icc_rpc_test(
                self.handle,
                value,
                icc_client_type_ICC_TYPE_JOBMON,
                &mut retcode,
            );
            if ret != icc_retcode_ICC_SUCCESS {
                return Err(ProxyErr::new(format!(
                    "Failed to send test RPC to ICC {}",
                    ret
                )));
            }
            Ok(retcode)
        }

        pub unsafe fn notify_alarm(
            &mut self,
            source_exporter: &str,
            alarm: &ValueAlarmTrigger,
        ) -> Result<i32, ProxyErr> {
            let mut retcode = 0;

            let source = CString::new(source_exporter).unwrap();
            let name = CString::new(alarm.name.clone()).unwrap();
            let metric = CString::new(alarm.metric.clone()).unwrap();
            let operator = CString::new(alarm.operator.to_string()).unwrap();
            let value = alarm.current;
            let active = alarm.active as i32;
            let pretty = CString::new(alarm.pretty.clone()).unwrap();

            let ret = icc_rpc_metric_alert(
                self.handle,
                source.into_raw(),
                name.into_raw(),
                metric.into_raw(),
                operator.into_raw(),
                value,
                active,
                pretty.into_raw(),
                &mut retcode,
            );
            if ret != icc_retcode_ICC_SUCCESS {
                return Err(ProxyErr::new(format!(
                    "Failed to send test RPC to ICC {}",
                    ret
                )));
            }
            Ok(retcode)
        }
    }

    pub struct IccInterface {
        watcher_thread: JoinHandle<()>,
    }

    impl IccInterface {
        pub fn new(exporter: Arc<ExporterFactory>) -> IccInterface {
            // Here we start a thread to watch the ICC
            // and to sandbox the unsafe code in a client thread
            let th = thread::spawn(move || {
                loop {
                    log::info!("Attempting to start the IC client thread");
                    // Here the thread will attempt to connect to the IC
                    // every 10 seconds by watching the subthread actually connecting

                    let expref = exporter.clone();

                    let ic_thread = thread::spawn(move || {
                        if let Err(e) = IccInterface::start(expref) {
                            log::error!("Could not connect to the IC {}", e.to_string());
                        }
                    });

                    if ic_thread.join().is_err() {
                        log::error!("Failed to join IC client thread");
                    }

                    thread::sleep(Duration::from_secs(10));
                }
            });

            IccInterface { watcher_thread: th }
        }

        fn notify_alarms(
            client: &mut IccClient,
            exporter: &Arc<ExporterFactory>,
            states: &mut HashMap<(String, String), bool>,
        ) -> Result<(), ProxyErr> {
            let alarms = exporter.list_alarms();

            for source_exporter in alarms.keys() {
                let active_alarms: Vec<&ValueAlarmTrigger> = alarms
                    .get(source_exporter)
                    .unwrap()
                    .iter()
                    .filter(|v| v.active)
                    .collect();
                for alarm in active_alarms {
                    /* Notify only if state has changed */
                    let al_key = (source_exporter.clone(), alarm.name.clone());

                    let mut notify_alarm = false;

                    if let Some(prev_state) = states.get_mut(&al_key) {
                        if !*prev_state {
                            notify_alarm = true;
                            *prev_state = true
                        }
                    } else {
                        notify_alarm = true;
                        states.insert(al_key, true);
                    }

                    if notify_alarm {
                        unsafe { client.notify_alarm(source_exporter, alarm)? };
                    }
                }

                /* Make sure to flag inactive alarms */
                let inactive_alarms: Vec<&ValueAlarmTrigger> = alarms
                    .get(source_exporter)
                    .unwrap()
                    .iter()
                    .filter(|v| !v.active)
                    .collect();
                for inactive in inactive_alarms {
                    let al_key = (source_exporter.clone(), inactive.name.clone());

                    let mut notify_alarm = false;

                    if let Some(prev_state) = states.get_mut(&al_key) {
                        if *prev_state {
                            notify_alarm = true;
                            *prev_state = false;
                        }
                    }

                    if notify_alarm {
                        unsafe { client.notify_alarm(source_exporter, inactive)? };
                    }
                }
            }

            Ok(())
        }

        fn start(exporter: Arc<ExporterFactory>) -> Result<(), ProxyErr> {
            let mut client: IccClient = unsafe { IccClient::new()? };

            log::info!("Connected to the intelligent controller");

            let mut notify_state: HashMap<(String, String), bool> = HashMap::new();

            loop {
                /* This is the loop where we do our IC actions
                it is in a thread under a watcher thread
                as we can surive any crash and retry
                due to the unsafe nature of the C code */

                if let Err(e) =
                    IccInterface::notify_alarms(&mut client, &exporter, &mut notify_state)
                {
                    log::error!("Error sending alarm {}", e);
                    unsafe { client.close()? };
                    return Err(e);
                }

                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}
