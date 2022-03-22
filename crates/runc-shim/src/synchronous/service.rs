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
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use log::{debug, error};

use containerd_shim as shim;
use runc::options::{DeleteOpts, GlobalOpts, DEFAULT_COMMAND};
use shim::api::*;
use shim::error::{Error, Result};
use shim::event::Event;
use shim::monitor::{monitor_subscribe, Subject, Subscription, Topic};
use shim::protos::events::task::TaskExit;
use shim::protos::protobuf::{Message, SingularPtrField};
use shim::publisher::RemotePublisher;
use shim::util::{
    convert_to_timestamp, read_options, read_runtime, read_spec_from_file, timestamp, write_address,
};
use shim::{io_error, other_error, warn};
use shim::{spawn, Config, Context, ExitSignal, Shim, StartOpts};

use crate::common::{create_runc, ShimExecutor, GROUP_LABELS};
use crate::synchronous::container::{Container, Process};
use crate::synchronous::runc::{RuncContainer, RuncFactory};
use crate::synchronous::task::ShimTask;
use crate::synchronous::Service;

impl Shim for Service {
    type T = ShimTask<RuncFactory, RuncContainer>;

    fn new(_runtime_id: &str, id: &str, namespace: &str, _config: &mut Config) -> Self {
        Service {
            exit: Arc::new(ExitSignal::default()),
            id: id.to_string(),
            namespace: namespace.to_string(),
        }
    }

    fn start_shim(&mut self, opts: StartOpts) -> containerd_shim::Result<String> {
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

        let (child_id, address) = spawn(opts, &grouping, Vec::new())?;

        #[cfg(target_os = "linux")]
        containerd_shim::cgroup::set_cgroup_and_oom_score(child_id)?;

        write_address(&address)?;
        Ok(address)
    }

    #[cfg(not(feature = "async"))]
    fn delete_shim(&mut self) -> containerd_shim::Result<DeleteResponse> {
        let namespace = self.namespace.as_str();
        let bundle = current_dir().map_err(io_error!(e, "get current dir"))?;
        let opts = read_options(&bundle)?;
        let runtime = read_runtime(&bundle)?;

        let runc = create_runc(
            &*runtime,
            namespace,
            &bundle,
            &opts,
            Some(Arc::new(ShimExecutor::default())),
        )?;
        runc.delete(&self.id, Some(&DeleteOpts { force: true }))
            .unwrap_or_else(|e| warn!("failed to remove runc container: {}", e));
        let mut resp = DeleteResponse::new();
        // sigkill
        resp.exit_status = 137;
        resp.exited_at = SingularPtrField::some(timestamp()?);
        Ok(resp)
    }

    #[cfg(feature = "async")]
    fn delete_shim(&mut self) -> containerd_shim::Result<DeleteResponse> {
        Err(Error::Unimplemented("delete shim".to_string()))
    }

    fn wait(&mut self) {
        self.exit.wait();
    }

    fn create_task_service(&self, publisher: RemotePublisher) -> Self::T {
        let (tx, rx) = channel();
        let task = ShimTask::new(self.namespace.as_str(), Arc::clone(&self.exit), tx.clone());

        let s = monitor_subscribe(Topic::All).expect("monitor subscribe failed");
        self.process_exits(s, &task, tx);
        forward(publisher, self.namespace.to_string(), rx);
        task
    }
}

impl Service {
    pub fn process_exits(
        &self,
        s: Subscription,
        task: &ShimTask<RuncFactory, RuncContainer>,
        tx: Sender<(String, Box<dyn Message>)>,
    ) {
        let containers = task.containers.clone();
        std::thread::spawn(move || {
            for e in s.rx.iter() {
                if let Subject::Pid(pid) = e.subject {
                    debug!("receive exit event: {}", &e);
                    let exit_code = e.exit_code;
                    for (_k, cont) in containers.lock().unwrap().iter_mut() {
                        let bundle = cont.common.bundle.to_string();
                        // pid belongs to container init process
                        if cont.pid() == pid {
                            // kill all children process if the container has a private PID namespace
                            if cont.should_kill_all_on_exit(&bundle) {
                                cont.kill(None, 9, true).unwrap_or_else(|e| {
                                    error!("failed to kill init's children: {}", e)
                                });
                            }
                            // set exit for init process
                            cont.common.init.set_exited(exit_code);

                            // publish event
                            let (_, code, exited_at) = match cont.get_exit_info(None) {
                                Ok(info) => info,
                                Err(_) => break,
                            };

                            let ts = convert_to_timestamp(exited_at);
                            let event = TaskExit {
                                container_id: cont.id(),
                                id: cont.id(),
                                pid: cont.pid() as u32,
                                exit_status: code as u32,
                                exited_at: SingularPtrField::some(ts),
                                ..Default::default()
                            };
                            let topic = event.topic();
                            tx.send((topic.to_string(), Box::new(event)))
                                .unwrap_or_else(|e| warn!("send {} to publisher: {}", topic, e));

                            break;
                        }

                        // pid belongs to container common process
                        for (_exec_id, p) in cont.common.processes.iter_mut() {
                            // set exit for exec process
                            if p.pid() == pid {
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

fn forward(publisher: RemotePublisher, ns: String, rx: Receiver<(String, Box<dyn Message>)>) {
    std::thread::spawn(move || {
        for (topic, e) in rx.iter() {
            publisher
                .publish(Context::default(), &topic, &ns, e)
                .unwrap_or_else(|e| warn!("publish {} to containerd: {}", topic, e));
        }
    });
}
