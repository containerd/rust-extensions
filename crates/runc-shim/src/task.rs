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
use std::sync::{Arc, Mutex, Once};

use log::{debug, info};

use containerd_shim as shim;

use shim::protos::protobuf::well_known_types::Timestamp;
use shim::protos::protobuf::SingularPtrField;
use shim::util::IntoOption;
use shim::Error;
use shim::Task;
use shim::{api::*, ExitSignal};
use shim::{TtrpcContext, TtrpcResult};

use crate::container::{Container, ContainerFactory};

pub struct ShimTask<F, C> {
    pub containers: Arc<Mutex<HashMap<String, C>>>,
    factory: F,
    namespace: String,
    exit: Arc<ExitSignal>,
    /// Prevent multiple shutdown
    shutdown: Once,
}

impl<F, C> ShimTask<F, C>
where
    F: Default,
{
    pub fn new(ns: &str, exit: Arc<ExitSignal>) -> Self {
        Self {
            factory: Default::default(),
            containers: Arc::new(Mutex::new(Default::default())),
            namespace: ns.to_string(),
            exit,
            shutdown: Once::new(),
        }
    }
}

impl<F, C> Task for ShimTask<F, C>
where
    F: ContainerFactory<C>,
    C: Container,
{
    fn state(&self, _ctx: &TtrpcContext, req: StateRequest) -> TtrpcResult<StateResponse> {
        let containers = self.containers.lock().unwrap();
        let container = containers.get(req.id.as_str()).ok_or_else(|| {
            Error::NotFoundError(format!("can not find container by id {}", req.id.as_str()))
        })?;
        let exec_id = req.exec_id.as_str().none_if(|&x| x.is_empty());
        let resp = container.state(exec_id)?;
        Ok(resp)
    }

    fn create(
        &self,
        _ctx: &TtrpcContext,
        req: CreateTaskRequest,
    ) -> TtrpcResult<CreateTaskResponse> {
        info!("Create request for {:?}", &req);
        // Note: Get containers here is for getting the lock,
        // to make sure no other threads manipulate the containers metadata;
        let mut containers = self.containers.lock().unwrap();

        let ns = self.namespace.as_str();
        let id = req.id.as_str();

        let container = self.factory.create(ns, &req)?;
        let mut resp = CreateTaskResponse::new();
        resp.pid = container.pid() as u32;

        containers.insert(id.to_string(), container);
        info!("Create request for {} returns pid {}", id, resp.pid);
        Ok(resp)
    }

    fn start(&self, _ctx: &TtrpcContext, req: StartRequest) -> TtrpcResult<StartResponse> {
        info!("Start request for {:?}", &req);
        let mut containers = self.containers.lock().unwrap();
        let container = containers.get_mut(req.get_id()).ok_or_else(|| {
            Error::NotFoundError(format!("can not find container by id {}", req.get_id()))
        })?;
        let pid = container.start(req.exec_id.as_str().none_if(|&x| x.is_empty()))?;

        let mut resp = StartResponse::new();
        resp.pid = pid as u32;
        info!("Start request for {:?} returns pid {}", req, resp.get_pid());
        Ok(resp)
    }

    fn delete(&self, _ctx: &TtrpcContext, req: DeleteRequest) -> TtrpcResult<DeleteResponse> {
        info!("Delete request for {:?}", &req);
        let mut containers = self.containers.lock().unwrap();
        let container = containers.get_mut(req.get_id()).ok_or_else(|| {
            Error::NotFoundError(format!("can not find container by id {}", req.get_id()))
        })?;
        let exec_id_opt = req.get_exec_id().none_if(|x| x.is_empty());
        let (pid, exit_status, exited_at) = container.delete(exec_id_opt)?;
        if req.get_exec_id().is_empty() {
            containers.remove(req.id.as_str());
        }
        let mut resp = DeleteResponse::new();
        resp.set_exited_at(exited_at);
        resp.set_pid(pid as u32);
        resp.set_exit_status(exit_status);
        info!(
            "Delete request for {} {} returns {:?}",
            req.get_id(),
            req.get_exec_id(),
            resp
        );
        Ok(resp)
    }

    fn kill(&self, _ctx: &TtrpcContext, req: KillRequest) -> TtrpcResult<Empty> {
        info!("Kill request for {:?}", req);
        let mut containers = self.containers.lock().unwrap();
        let container = containers.get_mut(req.get_id()).ok_or_else(|| {
            Error::NotFoundError(format!("can not find container by id {}", req.get_id()))
        })?;
        container.kill(
            req.exec_id.as_str().none_if(|&x| x.is_empty()),
            req.signal,
            req.all,
        )?;
        info!("Kill request for {:?} returns successfully", req);
        Ok(Empty::new())
    }

    fn exec(&self, _ctx: &TtrpcContext, req: ExecProcessRequest) -> TtrpcResult<Empty> {
        info!("Exec request for {:?}", req);
        let mut containers = self.containers.lock().unwrap();
        let container = containers.get_mut(req.get_id()).ok_or_else(|| {
            Error::Other(format!("can not find container by id {}", req.get_id()))
        })?;
        container.exec(req)?;
        Ok(Empty::new())
    }

    fn resize_pty(&self, _ctx: &TtrpcContext, req: ResizePtyRequest) -> TtrpcResult<Empty> {
        debug!(
            "Resize pty request for container {}, exec_id: {}",
            &req.id, &req.exec_id
        );
        let mut containers = self.containers.lock().unwrap();
        let container = containers.get_mut(req.get_id()).ok_or_else(|| {
            Error::Other(format!("can not find container by id {}", req.get_id()))
        })?;
        container.resize_pty(
            req.get_exec_id().none_if(|&x| x.is_empty()),
            req.height,
            req.width,
        )?;
        Ok(Empty::new())
    }

    fn wait(&self, _ctx: &TtrpcContext, req: WaitRequest) -> TtrpcResult<WaitResponse> {
        info!("Wait request for {:?}", req);
        let mut containers = self.containers.lock().unwrap();
        let container = containers.get_mut(req.get_id()).ok_or_else(|| {
            Error::Other(format!("can not find container by id {}", req.get_id()))
        })?;
        let exec_id = req.exec_id.as_str().none_if(|&x| x.is_empty());
        let state = container.state(exec_id)?;
        if state.status != Status::RUNNING && state.status != Status::CREATED {
            let mut resp = WaitResponse::new();
            resp.exit_status = state.exit_status;
            resp.exited_at = state.exited_at;
            info!("Wait request for {:?} returns {:?}", req, &resp);
            return Ok(resp);
        }
        let rx = container.wait_channel(req.exec_id.as_str().none_if(|&x| x.is_empty()))?;
        // release the lock before waiting the channel
        drop(containers);

        rx.recv()
            .expect_err("wait channel should be closed directly");
        // get lock again.
        let mut containers = self.containers.lock().unwrap();
        let container = containers.get_mut(req.get_id()).ok_or_else(|| {
            Error::Other(format!("can not find container by id {}", req.get_id()))
        })?;
        let (_, code, exited_at) = container.get_exit_info(exec_id)?;
        let mut resp = WaitResponse::new();
        resp.exit_status = code as u32;
        let mut ts = Timestamp::new();
        if let Some(ea) = exited_at {
            ts.seconds = ea.unix_timestamp();
            ts.nanos = ea.nanosecond() as i32;
        }
        resp.exited_at = SingularPtrField::some(ts);
        info!("Wait request for {:?} returns {:?}", req, &resp);
        Ok(resp)
    }

    fn shutdown(&self, _ctx: &TtrpcContext, _req: ShutdownRequest) -> TtrpcResult<Empty> {
        debug!("Shutdown request");
        let containers = self.containers.lock().unwrap();
        if containers.len() > 0 {
            return Ok(Empty::new());
        }

        self.shutdown.call_once(|| {
            self.exit.signal();
        });

        Ok(Empty::default())
    }
}
