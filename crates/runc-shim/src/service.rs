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

use std::{env::current_dir, sync::Arc};

use ::runc::options::DeleteOpts;
use async_trait::async_trait;
use containerd_shim::{
    asynchronous::{
        monitor::{monitor_subscribe, monitor_unsubscribe, Subscription},
        publisher::RemotePublisher,
        spawn, ExitSignal, Shim,
    },
    event::Event,
    io_error,
    monitor::{Subject, Topic},
    protos::{events::task::TaskExit, protobuf::MessageDyn},
    util::{
        convert_to_timestamp, read_options, read_pid_from_file, read_runtime, read_spec, timestamp,
        write_str_to_file,
    },
    Config, Context, DeleteResponse, Error, Flags, StartOpts,
};
use log::{debug, error, warn};
use tokio::sync::mpsc::{channel, Receiver, Sender};

use crate::{
    common::{create_runc, has_shared_pid_namespace, ShimExecutor, GROUP_LABELS, INIT_PID_FILE},
    container::Container,
    processes::Process,
    runc::{RuncContainer, RuncFactory},
    task::TaskService,
};

pub(crate) struct Service {
    exit: Arc<ExitSignal>,
    id: String,
    namespace: String,
}

#[async_trait]
impl Shim for Service {
    type T = TaskService<RuncFactory, RuncContainer>;

    async fn new(_runtime_id: &str, args: &Flags, _config: &mut Config) -> Self {
        let exit = Arc::new(ExitSignal::default());
        // TODO: add publisher
        Service {
            exit,
            id: args.id.to_string(),
            namespace: args.namespace.to_string(),
        }
    }

    async fn start_shim(&mut self, opts: StartOpts) -> containerd_shim::Result<String> {
        let mut grouping = opts.id.clone();
        let spec = read_spec("").await?;
        match spec.annotations() {
            Some(annotations) => {
                for &label in GROUP_LABELS.iter() {
                    if let Some(value) = annotations.get(label) {
                        grouping = value.to_string();
                        break;
                    }
                }
            }
            None => {}
        }
        #[cfg(not(target_os = "linux"))]
        let thp_disabled = String::new();
        #[cfg(target_os = "linux")]
        // Our goal is to set thp disable = true on the shim side and then restore thp
        // disable before starting runc. So we only need to focus on the return value
        // of the function get_thp_disabled, which is Result<bool, i32>.
        let thp_disabled = match prctl::get_thp_disable() {
            Ok(x) => {
                // The return value of the function set_thp_disabled is Result<(), i32>,
                // we don't care if the setting is successful, because even if the
                // setting failed, we should not exit the shim process, therefore,
                // there is no need to pay attention to the set_thp_disabled function's
                // return value.
                let _ = prctl::set_thp_disable(true);
                x.to_string()
            }
            Err(_) => String::new(),
        };
        let vars: Vec<(&str, &str)> = vec![("THP_DISABLED", thp_disabled.as_str())];

        let address = spawn(opts, &grouping, vars).await?;
        write_str_to_file("address", &address).await?;
        Ok(address)
    }

    async fn delete_shim(&mut self) -> containerd_shim::Result<DeleteResponse> {
        let namespace = self.namespace.as_str();
        let bundle = current_dir().map_err(io_error!(e, "get current dir"))?;
        let opts = read_options(&bundle).await?;
        let runtime = read_runtime(&bundle).await?;

        let runc = create_runc(
            &runtime,
            namespace,
            &bundle,
            &opts,
            Some(Arc::new(ShimExecutor::default())),
        )?;
        let pid = read_pid_from_file(&bundle.join(INIT_PID_FILE)).await?;

        runc.delete(&self.id, Some(&DeleteOpts { force: true }))
            .await
            .unwrap_or_else(|e| warn!("failed to remove runc container: {}", e));
        let mut resp = DeleteResponse::new();
        // sigkill
        resp.set_exit_status(137);
        resp.set_exited_at(timestamp()?);
        resp.set_pid(pid as u32);
        Ok(resp)
    }

    async fn wait(&mut self) {
        self.exit.wait().await;
    }

    async fn create_task_service(&self, publisher: RemotePublisher) -> Self::T {
        let (tx, rx) = channel(128);
        let exit_clone = self.exit.clone();
        let task = TaskService::new(&self.namespace, exit_clone, tx.clone());
        let s = monitor_subscribe(Topic::Pid)
            .await
            .expect("monitor subscribe failed");
        process_exits(s, &task, tx).await;
        forward(publisher, self.namespace.to_string(), rx).await;
        task
    }
}

async fn process_exits(
    s: Subscription,
    task: &TaskService<RuncFactory, RuncContainer>,
    tx: Sender<(String, Box<dyn MessageDyn>)>,
) {
    let containers = task.containers.clone();
    let mut s = s;
    tokio::spawn(async move {
        while let Some(e) = s.rx.recv().await {
            if let Subject::Pid(pid) = e.subject {
                debug!("receive exit event: {}", &e);
                let exit_code = e.exit_code;
                let containers = containers.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    for (_k, cont) in containers.lock().await.iter_mut() {
                        let bundle = cont.bundle.to_string();
                        // pid belongs to container init process
                        if cont.init.pid == pid {
                            // kill all children process if the container has a private PID namespace
                            if should_kill_all_on_exit(&bundle).await {
                                cont.kill(None, 9, true).await.unwrap_or_else(|e| {
                                    error!("failed to kill init's children: {}", e)
                                });
                            }
                            // set exit for init process
                            cont.init.set_exited(exit_code).await;

                            // publish event
                            let (_, code, exited_at) = match cont.get_exit_info(None).await {
                                Ok(info) => info,
                                Err(_) => break,
                            };

                            let ts = convert_to_timestamp(exited_at);
                            let event = TaskExit {
                                container_id: cont.id.to_string(),
                                id: cont.id.to_string(),
                                pid: cont.pid().await as u32,
                                exit_status: code as u32,
                                exited_at: Some(ts).into(),
                                ..Default::default()
                            };
                            let topic = event.topic();
                            tx.send((topic.to_string(), Box::new(event)))
                                .await
                                .unwrap_or_else(|e| warn!("send {} to publisher: {}", topic, e));

                            break;
                        }

                        // pid belongs to container common process
                        if let Some(p) = cont.processes.values_mut().find(|p| p.pid == pid) {
                            p.set_exited(exit_code).await;
                        }
                    }
                });
            }
        }
        monitor_unsubscribe(s.id).await.unwrap_or_default();
    });
}

async fn forward(
    publisher: RemotePublisher,
    ns: String,
    mut rx: Receiver<(String, Box<dyn MessageDyn>)>,
) {
    tokio::spawn(async move {
        while let Some((topic, e)) = rx.recv().await {
            publisher
                .publish(Context::default(), &topic, &ns, e)
                .await
                .unwrap_or_else(|e| warn!("publish {} to containerd: {}", topic, e));
        }
    });
}

async fn should_kill_all_on_exit(bundle_path: &str) -> bool {
    match read_spec(bundle_path).await {
        Ok(spec) => has_shared_pid_namespace(&spec),
        Err(e) => {
            error!(
                "failed to read spec when call should_kill_all_on_exit: {}",
                e
            );
            false
        }
    }
}
