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

use std::env::current_dir;
use std::sync::Arc;

use async_trait::async_trait;
use log::{debug, error, warn};
use tokio::sync::mpsc::{channel, Receiver, Sender};

use ::runc::options::DeleteOpts;
use containerd_shim::asynchronous::container::Container;
use containerd_shim::asynchronous::monitor::{
    monitor_subscribe, monitor_unsubscribe, Subscription,
};
use containerd_shim::asynchronous::processes::Process;
use containerd_shim::asynchronous::publisher::RemotePublisher;
use containerd_shim::asynchronous::task::TaskService;
use containerd_shim::asynchronous::{spawn, ExitSignal, Shim};
use containerd_shim::event::Event;
use containerd_shim::monitor::{Subject, Topic};
use containerd_shim::protos::events::task::TaskExit;
use containerd_shim::protos::protobuf::{Message, SingularPtrField};
use containerd_shim::util::{convert_to_timestamp, timestamp};
use containerd_shim::util::{read_options, read_runtime, read_spec, write_str_to_file};
use containerd_shim::{io_error, Config, Context, DeleteResponse, Error, StartOpts};

use crate::asynchronous::runc::{RuncContainer, RuncFactory};
use crate::common::{create_runc, has_shared_pid_namespace};
use crate::common::{ShimExecutor, GROUP_LABELS};

mod runc;

pub(crate) struct Service {
    exit: Arc<ExitSignal>,
    id: String,
    namespace: String,
}

#[async_trait]
impl Shim for Service {
    type T = TaskService<RuncFactory, RuncContainer>;

    async fn new(_runtime_id: &str, id: &str, namespace: &str, _config: &mut Config) -> Self {
        let exit = Arc::new(ExitSignal::default());
        // TODO: add publisher
        Service {
            exit,
            id: id.to_string(),
            namespace: namespace.to_string(),
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

        let address = spawn(opts, &grouping, Vec::new()).await?;
        write_str_to_file("address", &address).await?;
        Ok(address)
    }

    async fn delete_shim(&mut self) -> containerd_shim::Result<DeleteResponse> {
        let namespace = self.namespace.as_str();
        let bundle = current_dir().map_err(io_error!(e, "get current dir"))?;
        let opts = read_options(&bundle).await?;
        let runtime = read_runtime(&bundle).await?;

        let runc = create_runc(
            &*runtime,
            namespace,
            &bundle,
            &opts,
            Some(Arc::new(ShimExecutor::default())),
        )?;

        runc.delete(&self.id, Some(&DeleteOpts { force: true }))
            .await
            .unwrap_or_else(|e| warn!("failed to remove runc container: {}", e));
        let mut resp = DeleteResponse::new();
        // sigkill
        resp.exit_status = 137;
        resp.exited_at = SingularPtrField::some(timestamp()?);
        Ok(resp)
    }

    async fn wait(&mut self) {
        self.exit.wait().await;
    }

    async fn create_task_service(&self, publisher: RemotePublisher) -> Self::T {
        let (tx, rx) = channel(128);
        let exit_clone = self.exit.clone();
        let task = TaskService::new(&*self.namespace, exit_clone, tx.clone());
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
    tx: Sender<(String, Box<dyn Message>)>,
) {
    let containers = task.containers.clone();
    let mut s = s;
    tokio::spawn(async move {
        while let Some(e) = s.rx.recv().await {
            if let Subject::Pid(pid) = e.subject {
                debug!("receive exit event: {}", &e);
                let exit_code = e.exit_code;
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
                            exited_at: SingularPtrField::some(ts),
                            ..Default::default()
                        };
                        let topic = event.topic();
                        tx.send((topic.to_string(), Box::new(event)))
                            .await
                            .unwrap_or_else(|e| warn!("send {} to publisher: {}", topic, e));

                        break;
                    }

                    // pid belongs to container common process
                    for (_exec_id, p) in cont.processes.iter_mut() {
                        // set exit for exec process
                        if p.pid == pid {
                            p.set_exited(exit_code).await;
                            // TODO: publish event
                            break;
                        }
                    }
                }
            }
        }
        monitor_unsubscribe(s.id).await.unwrap_or_default();
    });
}

async fn forward(
    publisher: RemotePublisher,
    ns: String,
    mut rx: Receiver<(String, Box<dyn Message>)>,
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
