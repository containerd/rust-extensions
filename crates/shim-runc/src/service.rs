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

use std::collections::HashMap;
use std::env;
use std::rc::Rc;
use std::sync::{Arc, Mutex, RwLock};

use containerd_shim as shim;
use containerd_shim_protos as protos;

use protos::shim::task::Status as TaskStatus;
use protos::shim::{
    empty::Empty,
    shim::{
        CreateTaskRequest, CreateTaskResponse, DeleteRequest, DeleteResponse, KillRequest,
        StartRequest, StartResponse, StateRequest, StateResponse, WaitRequest, WaitResponse,
    },
};
use protos::Events;
use shim::ttrpc::{Code, Error, Status};
use shim::{api, ExitSignal, RemotePublisher, TtrpcContext, TtrpcResult};

use cgroups_rs as cgroup;

use cgroup::Hierarchy;
use log::{error, info};
use once_cell::sync::Lazy;
use protobuf::well_known_types::Timestamp;
use protobuf::{Message, RepeatedField, SingularPtrField};
use runc::options::*;
use sys_mount::UnmountFlags;
use time::OffsetDateTime;
use ttrpc::context::Context;

use crate::container::{self, Container};
use crate::oom::{v2::WatcherV2, Watcher};
use crate::options::oci::Options;
use crate::process::state::ProcessState;
use crate::utils;

// group labels specifies how the shim groups services.
// currently supports a runc.v2 specific .group label and the
// standard k8s pod label.  Order matters in this list
const GROUP_LABELS: [&str; 2] = [
    "io.containerd.runc.v2.group",
    "io.kubernetes.cri.sandbox-id",
];

const RUN_DIR: &str = "/run/containerd/runc";
const TASK_DIR: &str = "/run/containerd/io.containerd.runtime.v2.task";

static CONTAINERS: Lazy<RwLock<HashMap<String, Container>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

#[derive(Clone)]
pub struct Service {
    /// Runtime id
    runtime_id: String,
    /// Container id
    id: String,
    exit: ExitSignal,
    namespace: String,
    event_manager: Arc<EventManager>,
}

struct EventManager {
    // unlike go's implementation, we don't need event_send_mutex
    // because publisher needs to be in mutex due to Sync trait bound.
    publisher: Arc<Mutex<shim::RemotePublisher>>,
    oom_watcher: Arc<dyn Watcher>,
}

impl Service {
    fn send_event(&self, topic: &str, event: impl Message) -> Result<(), shim::Error> {
        self.event_manager
            .send(topic.to_string(), self.namespace.clone(), event)
    }

    fn add_oom_event(&self, cg: Arc<dyn Hierarchy>) -> std::io::Result<()> {
        self.event_manager
            .add_oom_event(self.id.clone(), self.namespace.clone(), cg)
    }
}

impl EventManager {
    fn new(publisher: RemotePublisher) -> Self {
        // note that watcher never need to run() again
        // then this Service needs only immutable reference to Watcher.
        let publisher = Arc::new(Mutex::new(publisher));
        let oom_watcher = if cgroup::hierarchies::is_cgroup2_unified_mode() {
            let mut watcher = WatcherV2::new();
            watcher.run(publisher.clone()).unwrap();
            Arc::new(watcher) as Arc<dyn Watcher>
        } else {
            // FIXME: support cgroup v1 OOM watcher
            unimplemented!()
        };
        Self {
            publisher,
            oom_watcher,
        }
    }

    // due to publisher's implementation, we cannot publish event topic from tokio task
    // because RemotePublisher take args in static dispatch (impl Message) and such event
    // cannot be send via channel
    // then, this method just publish topic in blocking way, on the main thread.
    fn send(
        &self,
        topic: String,
        namespace: String,
        event: impl Message,
    ) -> Result<(), shim::Error> {
        let ctx = Context::default();
        self.publisher
            .lock()
            .unwrap()
            .publish(ctx, &topic, &namespace, event)
            .map_err(|e| e.into())
    }

    fn add_oom_event(
        &self,
        id: String,
        namespace: String,
        cg: Arc<dyn Hierarchy>,
    ) -> std::io::Result<()> {
        self.oom_watcher.add(id, namespace, cg)
    }
}

impl shim::Shim for Service {
    type Error = shim::Error;
    type T = Service;

    fn new(
        _runtime_id: &str,
        _id: &str,
        _namespace: &str,
        _publisher: shim::RemotePublisher,
        _config: &mut shim::Config,
    ) -> Self {
        let runtime_id = _runtime_id.to_string();
        let id = _id.to_string();
        let namespace = _namespace.to_string();
        let exit = ExitSignal::default();
        let event_manager = Arc::new(EventManager::new(_publisher));
        Self {
            runtime_id,
            id,
            namespace,
            exit,
            event_manager,
        }
    }

    #[cfg(target_os = "linux")]
    fn start_shim(&mut self, opts: shim::StartOpts) -> Result<String, shim::Error> {
        let address = shim::spawn(opts, Vec::new())?;
        Ok(address)
    }

    #[cfg(not(target_os = "linux"))]
    fn start_shim(&mut self, opts: shim::StartOpts) -> Result<String, shim::Error> {
        let address = shim::spawn(opts, Vec::new())?;
        Err(shim::Error::Start(
            "non-linux implementation is not supported now.",
        ))
    }

    fn wait(&mut self) {
        self.exit.wait();
    }

    fn get_task_service(&self) -> Self::T {
        self.clone()
    }

    /// Cleaning up all containers in blocking way, when `shim delete` is invoked.
    fn delete_shim(&mut self) -> Result<DeleteResponse, Self::Error> {
        let cwd = env::current_dir()?;
        let parent = cwd
            .parent()
            .expect("Invalid: shim running on root directory.");
        let path = parent.join(&self.id);
        let opts =
            container::read_options(&path).map_err(|e| Self::Error::Delete(e.to_string()))?;
        let root = match opts {
            Some(Options { root, .. }) if !root.is_empty() => root,
            _ => RUN_DIR.to_string(),
        };
        let runc = utils::new_runc(&root, &path, self.namespace.clone(), "", false)
            .map_err(|e| Self::Error::Delete(e.to_string()))?;
        let opts = DeleteOpts { force: true };
        runc.delete(&self.id, Some(&opts))
            .map_err(|e| Self::Error::Delete(e.to_string()))?;

        sys_mount::unmount(&path.as_path().join("rootfs"), UnmountFlags::empty()).map_err(|e| {
            error!("failed to cleanup rootfs mount");
            Self::Error::Delete(e.to_string())
        })?;

        let now = OffsetDateTime::now_utc();
        let now = Some(Timestamp {
            seconds: now.unix_timestamp(),
            nanos: now.nanosecond() as i32,
            ..Default::default()
        });
        let exited_at = SingularPtrField::from_option(now);

        Ok(DeleteResponse {
            exited_at,
            exit_status: 137, // SIGKILL + 128
            ..Default::default()
        })
    }
}

impl shim::Task for Service {
    fn create(
        &self,
        _ctx: &shim::TtrpcContext,
        _req: CreateTaskRequest,
    ) -> shim::ttrpc::Result<CreateTaskResponse> {
        let id = _req.id.clone();
        let unknown_fields = _req.unknown_fields.clone();
        let cached_size = _req.cached_size.clone();
        // FIXME: error handling
        let container = match Container::new(_req.clone()) {
            Ok(c) => c,
            Err(e) => {
                return Err(Error::Others(format!(
                    "container create failed: id={}, err={}",
                    id, e
                )));
            }
        };
        let mut c = CONTAINERS.write().unwrap();
        let pid = container.pid() as u32;

        #[allow(clippy::map_entry)]
        if c.contains_key(&id) {
            return Err(Error::Others(format!(
                "create: container \"{}\" already exists.",
                id
            )));
        } else {
            let _ = c.insert(id, container);
        }

        let task_io = SingularPtrField::from_option(Some(protos::events::task::TaskIO {
            stdin: _req.stdin,
            stdout: _req.stdout,
            stderr: _req.stderr,
            terminal: _req.terminal,
            ..Default::default()
        }));

        // this might be redundant but we have to do this
        // due to duplicate mount.rs in protos::events and protos::shim
        let rootfs = _req
            .rootfs
            .into_iter()
            .map(|m| {
                let protos::shim::mount::Mount {
                    field_type,
                    source,
                    target,
                    options,
                    unknown_fields,
                    cached_size,
                } = m;
                protos::events::mount::Mount {
                    field_type,
                    source,
                    target,
                    options,
                    unknown_fields,
                    cached_size,
                }
            })
            .collect();

        if let Err(e) = self.send_event(
            protos::topics::TASK_CREATE_EVENT_TOPIC,
            protos::events::task::TaskCreate {
                container_id: _req.id,
                bundle: _req.bundle,
                rootfs,
                io: task_io,
                checkpoint: _req.checkpoint,
                pid: pid as u32,
                ..Default::default()
            },
        ) {
            error!("failed to publish TaskCreate: {}", e);
        }

        Ok(CreateTaskResponse {
            pid,
            unknown_fields,
            cached_size,
        })
    }

    fn start(
        &self,
        _ctx: &shim::TtrpcContext,
        _req: StartRequest,
    ) -> shim::ttrpc::Result<StartResponse> {
        let mut c = CONTAINERS.write().unwrap();
        let container = c.get_mut(_req.get_id()).ok_or_else(|| {
            Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: "container not created".to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        let pid = container.start(&_req).map_err(|_|
            // FIXME: appropriate error mapping
            Error::RpcStatus(Status {
                code: Code::UNKNOWN,
                message: "couldn't start container process.".to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
        }))?;

        if _req.exec_id.is_empty() {
            let cg = container.cgroup().unwrap();
            if cg.v2() {
                // FIXME: appropreate controling
                if let Err(e) = self.add_oom_event(cg) {
                    error!("failed to add OOM event: {}", e);
                }
            } else {
                unimplemented!()
            }
            if let Err(e) = self.send_event(
                protos::topics::TASK_START_EVENT_TOPIC,
                protos::events::task::TaskStart {
                    container_id: container.id(),
                    pid: pid as u32,
                    ..Default::default()
                },
            ) {
                error!("failed to publish TaskStart: {}", e);
            }
        } else {
            #[allow(clippy::collapsible_else_if)]
            if let Err(e) = self.send_event(
                protos::topics::TASK_EXEC_STARTED_EVENT_TOPIC,
                protos::events::task::TaskExecStarted {
                    container_id: container.id(),
                    exec_id: _req.exec_id,
                    pid: pid as u32,
                    ..Default::default()
                },
            ) {
                error!("failed to publish TaskExecStarted: {}", e);
            };
        }
        Ok(StartResponse {
            pid: pid as u32,
            unknown_fields: _req.unknown_fields,
            cached_size: _req.cached_size,
        })
    }

    fn state(
        &self,
        _ctx: &shim::TtrpcContext,
        _req: StateRequest,
    ) -> shim::ttrpc::Result<StateResponse> {
        let c = CONTAINERS.write().unwrap();
        let container = c.get(_req.get_id()).ok_or_else(|| {
            Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: "container not created".to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        let exec_id = _req.get_exec_id();
        let p = container.process(exec_id).map_err(|_| {
            Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: format!("process {} doesn't exist.", exec_id),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        #[rustfmt::skip]
        let status = match p.state {
            ProcessState::Unknown   => TaskStatus::UNKNOWN,
            ProcessState::Created   => TaskStatus::CREATED,
            ProcessState::Running   => TaskStatus::RUNNING,
            ProcessState::Stopped |
            ProcessState::Deleted   => TaskStatus::STOPPED,
            ProcessState::Paused    => TaskStatus::PAUSED,
            ProcessState::Pausing   => TaskStatus::PAUSING,
        };

        let stdio = p.stdio();
        let exited_at = p.exited_at().map(|t| Timestamp {
            seconds: t.unix_timestamp(),
            nanos: t.nanosecond() as i32,
            ..Default::default()
        });

        let exited_at = SingularPtrField::from_option(exited_at);
        Ok(StateResponse {
            id: _req.id,
            bundle: p.bundle.clone(),
            pid: p.pid() as u32,
            status,
            stdin: stdio.stdin,
            stdout: stdio.stdout,
            stderr: stdio.stderr,
            terminal: stdio.terminal,
            exit_status: p.exit_status() as u32,
            exited_at,
            exec_id: _req.exec_id,
            unknown_fields: _req.unknown_fields,
            cached_size: _req.cached_size,
        })
    }

    fn wait(
        &self,
        _ctx: &shim::TtrpcContext,
        _req: WaitRequest,
    ) -> shim::ttrpc::Result<WaitResponse> {
        let mut c = CONTAINERS.write().unwrap();
        let container = c.get_mut(_req.get_id()).ok_or_else(|| {
            Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: "container not created".to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        let exec_id = _req.get_exec_id();
        let p = container.process_mut(exec_id).map_err(|_| {
            Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: format!("process {} doesn't exist.", exec_id),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        p.wait().map_err(|e| {
            Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: format!("process {} failed: {}", exec_id, e),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        let exited_at = p.exited_at().map(|t| Timestamp {
            seconds: t.unix_timestamp(),
            nanos: t.nanosecond() as i32,
            ..Default::default()
        });

        Ok(WaitResponse {
            exit_status: p.exit_status() as u32,
            exited_at: SingularPtrField::from_option(exited_at),
            unknown_fields: _req.unknown_fields,
            cached_size: _req.cached_size,
        })
    }

    fn kill(&self, _ctx: &shim::TtrpcContext, _req: KillRequest) -> shim::ttrpc::Result<Empty> {
        let mut c = CONTAINERS.write().unwrap();
        let container = c.get_mut(_req.get_id()).ok_or_else(|| {
            Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: "container not created".to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        container.kill(&_req).map_err(|e| {
            Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: format!("failed to kill the container {}: {}", _req.id, e),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        Ok(containerd_shim_protos::shim::empty::Empty {
            unknown_fields: _req.unknown_fields,
            cached_size: _req.cached_size,
        })
    }

    fn delete(
        &self,
        _ctx: &shim::TtrpcContext,
        _req: DeleteRequest,
    ) -> shim::ttrpc::Result<DeleteResponse> {
        let mut c = CONTAINERS.write().unwrap();
        let container = c.get_mut(_req.get_id()).ok_or_else(|| {
            Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: "container not created".to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        match container.delete(&_req) {
            Ok((pid, exit_status, exited_at)) => {
                let exited_at = exited_at.map(|t| Timestamp {
                    seconds: t.unix_timestamp(),
                    nanos: t.nanosecond() as i32,
                    ..Default::default()
                });

                if let Err(e) = self.send_event(
                    protos::topics::TASK_DELETE_EVENT_TOPIC,
                    protos::events::task::TaskDelete {
                        container_id: container.id(),
                        pid: pid as u32,
                        exit_status: exit_status as u32,
                        exited_at: SingularPtrField::from_option(exited_at.clone()),
                        ..Default::default()
                    },
                ) {
                    error!("failed to publish TaskDelete: {}", e);
                };

                Ok(DeleteResponse {
                    pid: pid as u32,
                    exit_status: exit_status as u32,
                    exited_at: SingularPtrField::from_option(exited_at),
                    unknown_fields: _req.unknown_fields,
                    cached_size: _req.cached_size,
                })
            }
            _ => Err(Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: "failed to delete container.".to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields,
                cached_size: _req.cached_size,
            })),
        }
    }

    fn connect(
        &self,
        _ctx: &TtrpcContext,
        _req: api::ConnectRequest,
    ) -> TtrpcResult<api::ConnectResponse> {
        info!("Connect request");
        Ok(api::ConnectResponse {
            version: self.runtime_id.clone(),
            ..Default::default()
        })
    }

    fn shutdown(&self, _ctx: &TtrpcContext, _req: api::ShutdownRequest) -> TtrpcResult<Empty> {
        info!("Shutdown request");
        self.exit.signal();
        Ok(Empty::default())
    }
}
