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

use std::collections::HashMap;

use async_trait::async_trait;
use log::debug;
use time::OffsetDateTime;
use tokio::sync::oneshot::Receiver;

use containerd_shim_protos::api::{CreateTaskRequest, ExecProcessRequest, StateResponse};
use containerd_shim_protos::protobuf::well_known_types::Timestamp;

use crate::asynchronous::processes::Process;
use crate::error::Result;
use crate::Error;

#[async_trait]
pub trait Container {
    async fn start(&mut self, exec_id: Option<&str>) -> Result<i32>;
    async fn state(&self, exec_id: Option<&str>) -> Result<StateResponse>;
    async fn kill(&mut self, exec_id: Option<&str>, signal: u32, all: bool) -> Result<()>;
    async fn wait_channel(&mut self, exec_id: Option<&str>) -> Result<Receiver<()>>;
    async fn get_exit_info(
        &self,
        exec_id: Option<&str>,
    ) -> Result<(i32, u32, Option<OffsetDateTime>)>;
    async fn delete(&mut self, exec_id_opt: Option<&str>) -> Result<(i32, u32, Timestamp)>;
    async fn exec(&mut self, req: ExecProcessRequest) -> Result<()>;
    async fn resize_pty(&mut self, exec_id: Option<&str>, height: u32, width: u32) -> Result<()>;
    async fn pid(&self) -> i32;
}

#[async_trait]
pub trait ContainerFactory<C> {
    async fn create(&self, ns: &str, req: &CreateTaskRequest) -> Result<C>;
    async fn cleanup(&self, ns: &str, c: &C) -> Result<()>;
}

#[async_trait]
pub trait ProcessFactory<E> {
    async fn create(&self, req: &ExecProcessRequest) -> Result<E>;
}

/// ContainerTemplate is a template struct to implement Container,
/// most of the methods can be delegated to either init process or exec process.
/// that's why we provides a ContainerTemplate struct,
/// library users only need to implements Process for their own.
pub struct ContainerTemplate<T, E, P> {
    /// container id
    pub id: String,
    /// container bundle path
    pub bundle: String,
    /// init process of this container
    pub init: T,
    /// process factory that create processes when exec
    pub process_factory: P,
    /// exec processes of this container
    pub processes: HashMap<String, E>,
}

#[async_trait]
impl<T, E, P> Container for ContainerTemplate<T, E, P>
where
    T: Process + Send + Sync,
    E: Process + Send + Sync,
    P: ProcessFactory<E> + Send + Sync,
{
    async fn start(&mut self, exec_id: Option<&str>) -> Result<i32> {
        let process = self.get_mut_process(exec_id)?;
        process.start().await?;
        Ok(process.pid().await)
    }

    async fn state(&self, exec_id: Option<&str>) -> Result<StateResponse> {
        let process = self.get_process(exec_id)?;
        let mut resp = process.state().await?;
        resp.bundle = self.bundle.to_string();
        debug!("container state: {:?}", resp);
        Ok(resp)
    }

    async fn kill(&mut self, exec_id: Option<&str>, signal: u32, all: bool) -> Result<()> {
        let process = self.get_mut_process(exec_id)?;
        process.kill(signal, all).await
    }

    async fn wait_channel(&mut self, exec_id: Option<&str>) -> Result<Receiver<()>> {
        let process = self.get_mut_process(exec_id)?;
        process.wait_channel().await
    }

    async fn get_exit_info(
        &self,
        exec_id: Option<&str>,
    ) -> Result<(i32, u32, Option<OffsetDateTime>)> {
        let process = self.get_process(exec_id)?;
        Ok((
            process.pid().await,
            process.exit_code().await as u32,
            process.exited_at().await,
        ))
    }

    async fn delete(&mut self, exec_id_opt: Option<&str>) -> Result<(i32, u32, Timestamp)> {
        let (pid, code, exit_at) = self.get_exit_info(exec_id_opt).await?;
        let mut timestamp = Timestamp::new();
        if let Some(exit_at) = exit_at {
            timestamp.set_seconds(exit_at.unix_timestamp());
            timestamp.set_nanos(exit_at.nanosecond() as i32);
        }
        let process = self.get_mut_process(exec_id_opt);
        match process {
            Ok(p) => p.delete().await?,
            Err(Error::NotFoundError(_)) => return Ok((pid, code, timestamp)),
            Err(e) => return Err(e),
        }
        if let Some(exec_id) = exec_id_opt {
            self.processes.remove(exec_id);
        }
        Ok((pid, code, timestamp))
    }

    async fn exec(&mut self, req: ExecProcessRequest) -> Result<()> {
        let exec_id = req.exec_id.to_string();
        let exec_process = self.process_factory.create(&req).await?;
        self.processes.insert(exec_id, exec_process);
        Ok(())
    }

    async fn resize_pty(&mut self, exec_id: Option<&str>, height: u32, width: u32) -> Result<()> {
        let process = self.get_mut_process(exec_id)?;
        process.resize_pty(height, width).await
    }

    async fn pid(&self) -> i32 {
        self.init.pid().await
    }
}

impl<T, E, P> ContainerTemplate<T, E, P>
where
    T: Process + Send + Sync,
    E: Process + Send + Sync,
{
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
                    Error::NotFoundError("can not find the exec by id".to_string())
                })?;
                Ok(p)
            }
            None => Ok(&mut self.init),
        }
    }
}
