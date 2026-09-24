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

use std::{collections::HashMap, sync::Arc};

use containerd_shim::{
    api::Status,
    error::Result,
    protos::{
        api::{CreateTaskRequest, ExecProcessRequest, ProcessInfo, StateResponse},
        cgroups::metrics::Metrics,
        protobuf::{well_known_types::any::Any, EnumOrUnknown, Message, MessageDyn},
        shim::oci::ProcessDetails,
    },
    Error,
};
use log::debug;
use oci_spec::runtime::LinuxResources;
use runc::Spawner;
use time::OffsetDateTime;
use tokio::sync::oneshot::Receiver;

use crate::{
    common::get_spec_from_request,
    io::Stdio,
    processes::Process,
    runc::{prepare_bundle, ExecProcess, InitProcess, RuncInitLifecycle},
};

/// A container managed by this shim: its init process plus any processes
/// exec'd into it.
pub struct Container {
    /// init process of this container
    pub init: InitProcess,
    /// exec processes of this container
    pub processes: HashMap<String, ExecProcess>,
}

impl Container {
    /// Creates the container through the OCI runtime, launched via `spawner`.
    pub async fn create(
        ns: &str,
        req: &CreateTaskRequest,
        spawner: Arc<dyn Spawner + Send + Sync>,
    ) -> Result<Self> {
        let (runc, opts) = prepare_bundle(ns, req, spawner).await?;
        let stdio = Stdio::new(req.stdin(), req.stdout(), req.stderr(), req.terminal());
        let lifecycle = RuncInitLifecycle::new(runc, opts, req.bundle());
        let init = InitProcess::create(req.id(), stdio, lifecycle).await?;
        Ok(Self {
            init,
            processes: HashMap::new(),
        })
    }

    /// The container id, which is also the id of its init process.
    pub fn id(&self) -> &str {
        &self.init.id
    }

    pub fn bundle(&self) -> &str {
        self.init.lifecycle.bundle()
    }

    pub async fn init_state(&self) -> EnumOrUnknown<Status> {
        // Default should be unknown
        self.init.state().await.unwrap_or_default().status
    }

    pub async fn start(&mut self, exec_id: Option<&str>) -> Result<i32> {
        let process = self.get_mut_process(exec_id)?;
        process.start().await?;
        Ok(process.pid().await)
    }

    pub async fn state(&self, exec_id: Option<&str>) -> Result<StateResponse> {
        let process = self.get_process(exec_id)?;
        let mut resp = process.state().await?;
        let init_state = self.init.state().await?.status;
        if init_state == EnumOrUnknown::new(Status::PAUSING)
            || init_state == EnumOrUnknown::new(Status::PAUSED)
        {
            resp.status = init_state;
        }
        resp.bundle = self.bundle().to_string();
        debug!("container state: {:?}", resp);
        Ok(resp)
    }

    pub async fn kill(&mut self, exec_id: Option<&str>, signal: u32, all: bool) -> Result<()> {
        let process = self.get_mut_process(exec_id)?;
        process.kill(signal, all).await
    }

    pub async fn wait_channel(&mut self, exec_id: Option<&str>) -> Result<Receiver<()>> {
        let process = self.get_mut_process(exec_id)?;
        process.wait_channel().await
    }

    pub async fn get_exit_info(
        &self,
        exec_id: Option<&str>,
    ) -> Result<(i32, i32, Option<OffsetDateTime>)> {
        let process = self.get_process(exec_id)?;
        Ok((
            process.pid().await,
            process.exit_code().await,
            process.exited_at().await,
        ))
    }

    pub async fn delete(
        &mut self,
        exec_id_opt: Option<&str>,
    ) -> Result<(i32, i32, Option<OffsetDateTime>)> {
        let (pid, code, exited_at) = self.get_exit_info(exec_id_opt).await?;
        self.get_mut_process(exec_id_opt)?.delete().await?;
        if let Some(exec_id) = exec_id_opt {
            self.processes.remove(exec_id);
        }
        Ok((pid, code, exited_at))
    }

    pub fn exec(&mut self, req: ExecProcessRequest) -> Result<()> {
        let spec = get_spec_from_request(&req)?;
        let stdio = Stdio::new(&req.stdin, &req.stdout, &req.stderr, req.terminal);
        let lifecycle = self.init.lifecycle.exec_lifecycle(self.id(), spec);
        let exec_process = ExecProcess::new(&req.exec_id, stdio, lifecycle);
        self.processes.insert(req.exec_id, exec_process);
        Ok(())
    }

    pub async fn resize_pty(
        &mut self,
        exec_id: Option<&str>,
        height: u32,
        width: u32,
    ) -> Result<()> {
        let process = self.get_mut_process(exec_id)?;
        process.resize_pty(height, width).await
    }

    pub async fn pid(&self) -> i32 {
        self.init.pid().await
    }

    #[cfg(target_os = "linux")]
    pub async fn update(&mut self, resources: &LinuxResources) -> Result<()> {
        self.init.update(resources).await
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn update(&mut self, _resources: &LinuxResources) -> Result<()> {
        Err(Error::Unimplemented("update".to_string()))
    }

    #[cfg(target_os = "linux")]
    pub async fn stats(&self) -> Result<Metrics> {
        self.init.stats().await
    }

    #[cfg(not(target_os = "linux"))]
    pub async fn stats(&self) -> Result<Metrics> {
        Err(Error::Unimplemented("stats".to_string()))
    }

    pub async fn all_processes(&self) -> Result<Vec<ProcessInfo>> {
        let mut processes_info = self.init.ps().await?;
        for process_info in &mut processes_info {
            for (exec_id, process) in &self.processes {
                if process_info.pid as i32 == process.pid().await {
                    let process_details = ProcessDetails {
                        exec_id: exec_id.to_string(),
                        special_fields: Default::default(),
                    };
                    let v = Any {
                        type_url: process_details.descriptor_dyn().full_name().to_string(),
                        value: process_details.write_to_bytes()?,
                        special_fields: Default::default(),
                    };
                    process_info.set_info(v);
                    break;
                }
            }
        }
        Ok(processes_info)
    }

    pub async fn close_io(&mut self, exec_id: Option<&str>) -> Result<()> {
        let process = self.get_mut_process(exec_id)?;
        process.close_io().await
    }

    pub async fn pause(&mut self) -> Result<()> {
        self.init.pause().await
    }

    pub async fn resume(&mut self) -> Result<()> {
        self.init.resume().await
    }

    pub fn get_process(&self, exec_id: Option<&str>) -> Result<&(dyn Process + Send + Sync)> {
        match exec_id {
            Some(exec_id) => {
                let p = self.processes.get(exec_id).ok_or_else(|| {
                    Error::NotFoundError("can not find the exec by id".to_string())
                })?;
                Ok(p)
            }
            None => Ok(&self.init),
        }
    }

    pub fn get_mut_process(
        &mut self,
        exec_id: Option<&str>,
    ) -> Result<&mut (dyn Process + Send + Sync)> {
        match exec_id {
            Some(exec_id) => {
                let p = self.processes.get_mut(exec_id).ok_or_else(|| {
                    Error::NotFoundError(format!("can not find the exec by id {}", exec_id))
                })?;
                Ok(p)
            }
            None => Ok(&mut self.init),
        }
    }
}
