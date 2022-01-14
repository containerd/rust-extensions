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
use std::sync::RwLock;

use containerd_runc_rust as runc;
use containerd_shim as shim;
use containerd_shim_protos as protos;

use protos::shim::task::Status as TaskStatus;
use protos::shim::{
    empty::Empty,
    shim::{
        CreateTaskRequest, CreateTaskResponse, DeleteRequest, DeleteResponse, ExecProcessRequest,
        KillRequest, StartRequest, StartResponse, StateRequest, StateResponse, WaitRequest,
        WaitResponse,
    },
};
use runc::options::*;
use shim::{api, ExitSignal, TtrpcContext, TtrpcResult};

use chrono::Utc;
use log::{error, info};
use once_cell::sync::Lazy;
use protobuf::well_known_types::Timestamp;
use protobuf::{RepeatedField, SingularPtrField};
use sys_mount::UnmountFlags;
use ttrpc::{Code, Status};

use crate::container::{self, Container};
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
    namespace: String,
    exit: ExitSignal,
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
        Self {
            runtime_id,
            id,
            namespace,
            exit,
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
            Some(Options { root, .. }) if root != "" => root,
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

        let now = Utc::now();
        let now = Some(Timestamp {
            seconds: now.timestamp(),
            nanos: (now.timestamp_nanos() % 1_000_000) as i32,
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
        _ctx: &ttrpc::TtrpcContext,
        _req: CreateTaskRequest,
    ) -> ttrpc::Result<CreateTaskResponse> {
        let id = _req.id.clone();
        let unknown_fields = _req.unknown_fields.clone();
        let cached_size = _req.cached_size.clone();
        // FIXME: error handling
        let container = match Container::new(_req) {
            Ok(c) => c,
            Err(e) => {
                return Err(ttrpc::Error::Others(format!(
                    "container create failed: id={}, err={}",
                    id, e
                )));
            }
        };
        let mut c = CONTAINERS.write().unwrap();
        let pid = container.pid() as u32;
        if c.contains_key(&id) {
            return Err(ttrpc::Error::Others(format!(
                "create: container \"{}\" already exists.",
                id
            )));
        } else {
            let _ = c.insert(id, container);
        }
        Ok(CreateTaskResponse {
            pid,
            unknown_fields,
            cached_size,
        })
    }

    fn start(
        &self,
        _ctx: &ttrpc::TtrpcContext,
        _req: StartRequest,
    ) -> ttrpc::Result<StartResponse> {
        let mut c = CONTAINERS.write().unwrap();

        let container = c.get_mut(_req.get_id()).ok_or_else(|| {
            ttrpc::Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: "container not created".to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;
        let pid = container.start(&_req).map_err(|_|
            // FIXME: appropriate error mapping
            ttrpc::error::Error::RpcStatus(Status {
                code: Code::UNKNOWN,
                message: "couldn't start container process.".to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
        }))?;
        Ok(StartResponse {
            pid: pid as u32,
            unknown_fields: _req.unknown_fields,
            cached_size: _req.cached_size,
        })
    }

    fn exec(
        &self,
        _ctx: &::ttrpc::TtrpcContext,
        _req: ExecProcessRequest,
    ) -> ::ttrpc::Result<Empty> {
        Err(::ttrpc::Error::RpcStatus(::ttrpc::get_status(
            ::ttrpc::Code::NOT_FOUND,
            "/containerd.task.v2.Task/Exec is not supported".to_string(),
        )))
    }

    fn state(
        &self,
        _ctx: &ttrpc::TtrpcContext,
        _req: StateRequest,
    ) -> ttrpc::Result<StateResponse> {
        let c = CONTAINERS.write().unwrap();
        let container = c.get(_req.get_id()).ok_or_else(|| {
            ttrpc::Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: "container not created".to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        let exec_id = _req.get_exec_id();
        let p = container.process(exec_id).map_err(|_| {
            ttrpc::Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: format!("process {} doesn't exist.", exec_id).to_string(),
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
        let exited_at = if let Some(exited_at) = p.exited_at() {
            Some(Timestamp {
                seconds: exited_at.timestamp(),
                nanos: (exited_at.timestamp_nanos() % 1_000_000) as i32,
                ..Default::default()
            })
        } else {
            None
        };
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
            ..Default::default()
        })
    }

    fn wait(&self, _ctx: &ttrpc::TtrpcContext, _req: WaitRequest) -> ttrpc::Result<WaitResponse> {
        let mut c = CONTAINERS.write().unwrap();
        let container = c.get_mut(_req.get_id()).ok_or_else(|| {
            ttrpc::Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: "container not created".to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        let exec_id = _req.get_exec_id();
        let p = container.process_mut(exec_id).map_err(|_| {
            ttrpc::Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: format!("process {} doesn't exist.", exec_id).to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;
        p.wait().map_err(|e| {
            ttrpc::Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: format!("process {} failed: {}", exec_id, e).to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        let exited_at = match p.exited_at() {
            Some(t) => Some(Timestamp {
                seconds: t.timestamp(),
                nanos: (t.timestamp_nanos() % 1_000_000) as i32,
                ..Default::default()
            }),
            None => None,
        };
        Ok(WaitResponse {
            exit_status: p.exit_status() as u32,
            exited_at: SingularPtrField::from_option(exited_at),
            unknown_fields: _req.unknown_fields,
            cached_size: _req.cached_size,
        })
    }

    fn kill(&self, _ctx: &ttrpc::TtrpcContext, _req: KillRequest) -> ttrpc::Result<Empty> {
        let mut c = CONTAINERS.write().unwrap();
        let container = c.get_mut(_req.get_id()).ok_or_else(|| {
            ttrpc::Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: "container not created".to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        container.kill(&_req).map_err(|e| {
            ttrpc::Error::RpcStatus(Status {
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
        _ctx: &ttrpc::TtrpcContext,
        _req: DeleteRequest,
    ) -> ttrpc::Result<DeleteResponse> {
        let mut c = CONTAINERS.write().unwrap();
        let container = c.get_mut(_req.get_id()).ok_or_else(|| {
            ttrpc::Error::RpcStatus(Status {
                code: Code::NOT_FOUND,
                message: "container not created".to_string(),
                details: RepeatedField::new(),
                unknown_fields: _req.unknown_fields.clone(),
                cached_size: _req.cached_size.clone(),
            })
        })?;

        match container.delete(&_req) {
            Ok((pid, exit_status, exited_at)) => {
                // Might be ugly hack
                let exited_at = match exited_at {
                    Some(t) => Some(Timestamp {
                        seconds: t.timestamp(),
                        nanos: (t.timestamp_nanos() % 1_000_000) as i32,
                        ..Default::default()
                    }),
                    None => None,
                };

                Ok(DeleteResponse {
                    pid: pid as u32,
                    exit_status: exit_status as u32,
                    exited_at: SingularPtrField::from_option(exited_at),
                    unknown_fields: _req.unknown_fields,
                    cached_size: _req.cached_size,
                })
            }
            _ => Err(ttrpc::Error::RpcStatus(Status {
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
