/*
   Copyright The containerd Authors.

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
*/

#![allow(unused)]

use std::env::current_dir;
use std::path::Path;
use std::sync::{Arc, Mutex};

use containerd_shim as shim;

use shim::api::*;
use shim::error::{Error, Result};
use shim::monitor::{monitor_subscribe, Subject, Subscription, Topic};
use shim::protos::protobuf::SingularPtrField;
use shim::util::{get_timestamp, read_options, read_runtime, read_spec_from_file, write_address};
use shim::{debug, error, io_error, other_error, warn};
use shim::{spawn, Config, ExitSignal, RemotePublisher, Shim, StartOpts};

use runc::options::{DeleteOpts, GlobalOpts, DEFAULT_COMMAND};

use crate::container::{Container, Process};
use crate::runc::{RuncContainer, RuncFactory, DEFAULT_RUNC_ROOT};
use crate::task::ShimTask;

const GROUP_LABELS: [&str; 2] = [
    "io.containerd.runc.v2.group",
    "io.kubernetes.cri.sandbox-id",
];

pub(crate) struct Service {
    exit: ExitSignal,
    id: String,
    task: ShimTask<RuncFactory, RuncContainer>,
}

impl Shim for Service {
    type T = ShimTask<RuncFactory, RuncContainer>;

    fn new(
        _runtime_id: &str,
        id: &str,
        namespace: &str,
        _publisher: RemotePublisher,
        _config: &mut Config,
    ) -> Self {
        let exit = ExitSignal::default();
        let exit_clone = exit.clone();
        let shutdown_cb = Box::new(move || exit_clone.signal());

        let mut task = ShimTask::new(namespace);
        task.shutdown_cb = Arc::new(Mutex::new(Some(shutdown_cb)));

        let mut service = Service {
            exit,
            id: id.to_string(),
            task,
        };

        let s = monitor_subscribe(Topic::All).expect("monitor subscribe failed");
        service.process_exits(s);

        // TODO: add publisher

        service
    }

    fn start_shim(&mut self, opts: StartOpts) -> Result<String> {
        let mut grouping = opts.id.clone();
        let spec = read_spec_from_file("")?;
        match spec.annotations() {
            Some(annotations) => {
                for label in GROUP_LABELS.iter() {
                    if let Some(value) = annotations.get(*label) {
                        grouping = value.to_string();
                        break;
                    }
                }
            }
            None => {}
        }

        let address = spawn(opts, &grouping, Vec::new())?;
        write_address(&address)?;
        Ok(address)
    }

    #[cfg(not(feature = "async"))]
    fn delete_shim(&mut self) -> Result<DeleteResponse> {
        let namespace = self.task.namespace.as_str();
        let bundle_buf = current_dir().map_err(io_error!(e, "get current dir"))?;
        let bundle = bundle_buf.as_path().to_str().unwrap();
        let opts = read_options(bundle)?;
        let mut runtime = read_runtime(bundle)?;

        let runc = {
            if runtime.is_empty() {
                runtime = DEFAULT_COMMAND.to_string();
            }
            let root = opts.root.as_str();
            let root = Path::new(if root.is_empty() {
                DEFAULT_RUNC_ROOT
            } else {
                root
            })
            .join(namespace);
            let log_buf = Path::new(bundle).join("log.json");
            let log = log_buf.to_str().unwrap();
            GlobalOpts::default()
                .command(runtime)
                .root(root)
                .log(log)
                .log_json()
                .systemd_cgroup(opts.systemd_cgroup)
                .build()
                .map_err(other_error!(e, "unable to create runc instance"))?
        };
        runc.delete(&self.id, Some(&DeleteOpts { force: true }))
            .unwrap_or_else(|e| warn!("failed to remove runc container: {}", e));
        let mut resp = DeleteResponse::new();
        // sigkill
        resp.exit_status = 137;
        resp.exited_at = SingularPtrField::some(get_timestamp()?);
        Ok(resp)
    }

    #[cfg(feature = "async")]
    fn delete_shim(&mut self) -> Result<DeleteResponse> {
        Err(Error::Unimplemented("delete shim".to_string()))
    }

    fn wait(&mut self) {
        self.exit.wait();
    }

    fn get_task_service(&self) -> Self::T {
        self.task.clone()
    }
}

impl Service {
    pub fn process_exits(&mut self, s: Subscription) {
        let containers = self.task.containers.clone();
        std::thread::spawn(move || {
            for e in s.rx.iter() {
                if let Subject::Pid(pid) = e.subject {
                    debug!("receive exit event: {}", &e);
                    let exit_code = e.exit_code;
                    for (_k, cont) in containers.lock().unwrap().iter_mut() {
                        let bundle = cont.common.bundle.to_string();
                        // pid belongs to container init process
                        if cont.common.init.common.pid == pid {
                            // kill all children process if the container has a private PID namespace
                            if cont.should_kill_all_on_exit(&bundle) {
                                cont.kill(None, 9, true).unwrap_or_else(|e| {
                                    error!("failed to kill init's children: {}", e)
                                });
                            }
                            // set exit for init process
                            cont.common.init.set_exited(exit_code);
                            // TODO: publish event
                            break;
                        }

                        // pid belongs to container common process
                        for (_exec_id, p) in cont.common.processes.iter_mut() {
                            // set exit for exec process
                            if p.common.pid == pid {
                                p.set_exited(exit_code);
                                // TODO: publish event
                                break;
                            }
                        }
                    }
                }
            }
        });
    }
}
