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

use async_trait::async_trait;
use containerd_shim_protos::{
    api::{
        CloseIORequest, ConnectRequest, ConnectResponse, DeleteResponse, PidsRequest, PidsResponse,
        StatsRequest, StatsResponse, UpdateTaskRequest,
    },
    events::task::{TaskCreate, TaskDelete, TaskExecAdded, TaskExecStarted, TaskIO, TaskStart},
    protobuf::MessageDyn,
    shim_async::Task,
    ttrpc,
    ttrpc::r#async::TtrpcContext,
};
use log::{debug, info, warn};
use oci_spec::runtime::LinuxResources;
use tokio::sync::{mpsc::Sender, MappedMutexGuard, Mutex, MutexGuard};

use crate::{
    api::{
        CreateTaskRequest, CreateTaskResponse, DeleteRequest, Empty, ExecProcessRequest,
        KillRequest, ResizePtyRequest, ShutdownRequest, StartRequest, StartResponse, StateRequest,
        StateResponse, Status, WaitRequest, WaitResponse,
    },
    asynchronous::{
        container::{Container, ContainerFactory},
        ExitSignal,
    },
    event::Event,
    util::{convert_to_any, convert_to_timestamp, AsOption},
    TtrpcResult,
};

type EventSender = Sender<(String, Box<dyn MessageDyn>)>;

/// TaskService is a Task template struct, it is considered a helper struct,
/// which has already implemented `Task` trait, so that users can make it the type `T`
/// parameter of `Service`, and implements their own `ContainerFactory` and `Container`.
pub struct TaskService<F, C> {
    pub factory: F,
    pub containers: Arc<Mutex<HashMap<String, C>>>,
    pub namespace: String,
    pub exit: Arc<ExitSignal>,
    pub tx: EventSender,
}

impl<F, C> TaskService<F, C>
where
    F: Default,
{
    pub fn new(ns: &str, exit: Arc<ExitSignal>, tx: EventSender) -> Self {
        Self {
            factory: Default::default(),
            containers: Arc::new(Mutex::new(Default::default())),
            namespace: ns.to_string(),
            exit,
            tx,
        }
    }
}

impl<F, C> TaskService<F, C> {
    pub async fn get_container(&self, id: &str) -> TtrpcResult<MappedMutexGuard<'_, C>> {
        let mut containers = self.containers.lock().await;
        containers.get_mut(id).ok_or_else(|| {
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::NOT_FOUND,
                format!("can not find container by id {}", id),
            ))
        })?;
        let container = MutexGuard::map(containers, |m| m.get_mut(id).unwrap());
        Ok(container)
    }

    pub async fn send_event(&self, event: impl Event) {
        let topic = event.topic();
        self.tx
            .send((topic.to_string(), Box::new(event)))
            .await
            .unwrap_or_else(|e| warn!("send {} to publisher: {}", topic, e));
    }
}

#[async_trait]
impl<F, C> Task for TaskService<F, C>
where
    F: ContainerFactory<C> + Sync + Send,
    C: Container + Sync + Send + 'static,
{
    async fn state(&self, _ctx: &TtrpcContext, req: StateRequest) -> TtrpcResult<StateResponse> {
        let container = self.get_container(req.id()).await?;
        let exec_id = req.exec_id().as_option();
        let resp = container.state(exec_id).await?;
        Ok(resp)
    }

    async fn create(
        &self,
        _ctx: &TtrpcContext,
        req: CreateTaskRequest,
    ) -> TtrpcResult<CreateTaskResponse> {
        info!("Create request for {:?}", &req);
        // Note: Get containers here is for getting the lock,
        // to make sure no other threads manipulate the containers metadata;
        let mut containers = self.containers.lock().await;

        let ns = self.namespace.as_str();
        let id = req.id.as_str();

        let container = self.factory.create(ns, &req).await?;
        let mut resp = CreateTaskResponse::new();
        let pid = container.pid().await as u32;
        resp.pid = pid;

        containers.insert(id.to_string(), container);

        self.send_event(TaskCreate {
            container_id: req.id.to_string(),
            bundle: req.bundle.to_string(),
            rootfs: req.rootfs,
            io: Some(TaskIO {
                stdin: req.stdin.to_string(),
                stdout: req.stdout.to_string(),
                stderr: req.stderr.to_string(),
                terminal: req.terminal,
                ..Default::default()
            })
            .into(),
            checkpoint: req.checkpoint.to_string(),
            pid,
            ..Default::default()
        })
        .await;
        info!("Create request for {} returns pid {}", id, resp.pid);
        Ok(resp)
    }

    async fn start(&self, _ctx: &TtrpcContext, req: StartRequest) -> TtrpcResult<StartResponse> {
        info!("Start request for {:?}", &req);
        let mut container = self.get_container(req.id()).await?;
        let pid = container.start(req.exec_id.as_str().as_option()).await?;

        let mut resp = StartResponse::new();
        resp.pid = pid as u32;

        if req.exec_id.is_empty() {
            self.send_event(TaskStart {
                container_id: req.id.to_string(),
                pid: pid as u32,
                ..Default::default()
            })
            .await;
        } else {
            self.send_event(TaskExecStarted {
                container_id: req.id.to_string(),
                exec_id: req.exec_id.to_string(),
                pid: pid as u32,
                ..Default::default()
            })
            .await;
        };

        info!("Start request for {:?} returns pid {}", req, resp.pid());
        Ok(resp)
    }

    async fn delete(&self, _ctx: &TtrpcContext, req: DeleteRequest) -> TtrpcResult<DeleteResponse> {
        info!("Delete request for {:?}", &req);
        let mut containers = self.containers.lock().await;
        let container = containers.get_mut(req.id()).ok_or_else(|| {
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::NOT_FOUND,
                format!("can not find container by id {}", req.id()),
            ))
        })?;
        let id = container.id().await;
        let exec_id_opt = req.exec_id().as_option();
        let (pid, exit_status, exited_at) = container.delete(exec_id_opt).await?;
        self.factory.cleanup(&self.namespace, container).await?;
        if req.exec_id().is_empty() {
            containers.remove(req.id());
        }

        let ts = convert_to_timestamp(exited_at);
        self.send_event(TaskDelete {
            container_id: id,
            pid: pid as u32,
            exit_status: exit_status as u32,
            exited_at: Some(ts.clone()).into(),
            ..Default::default()
        })
        .await;

        let mut resp = DeleteResponse::new();
        resp.set_exited_at(ts);
        resp.set_pid(pid as u32);
        resp.set_exit_status(exit_status as u32);
        info!(
            "Delete request for {} {} returns {:?}",
            req.id(),
            req.exec_id(),
            resp
        );
        Ok(resp)
    }

    async fn pids(&self, _ctx: &TtrpcContext, req: PidsRequest) -> TtrpcResult<PidsResponse> {
        debug!("Pids request for {:?}", req);
        let container = self.get_container(req.id()).await?;
        let processes = container.all_processes().await?;
        debug!("Pids request for {:?} returns successfully", req);
        Ok(PidsResponse {
            processes,
            ..Default::default()
        })
    }

    async fn kill(&self, _ctx: &TtrpcContext, req: KillRequest) -> TtrpcResult<Empty> {
        info!("Kill request for {:?}", req);
        let mut container = self.get_container(req.id()).await?;
        container
            .kill(req.exec_id().as_option(), req.signal, req.all)
            .await?;
        info!("Kill request for {:?} returns successfully", req);
        Ok(Empty::new())
    }

    async fn exec(&self, _ctx: &TtrpcContext, req: ExecProcessRequest) -> TtrpcResult<Empty> {
        info!("Exec request for {:?}", req);
        let exec_id = req.exec_id().to_string();
        let mut container = self.get_container(req.id()).await?;
        container.exec(req).await?;

        self.send_event(TaskExecAdded {
            container_id: container.id().await,
            exec_id,
            ..Default::default()
        })
        .await;

        Ok(Empty::new())
    }

    async fn resize_pty(&self, _ctx: &TtrpcContext, req: ResizePtyRequest) -> TtrpcResult<Empty> {
        debug!(
            "Resize pty request for container {}, exec_id: {}",
            &req.id, &req.exec_id
        );
        let mut container = self.get_container(req.id()).await?;
        container
            .resize_pty(req.exec_id().as_option(), req.height, req.width)
            .await?;
        Ok(Empty::new())
    }

    async fn close_io(&self, _ctx: &TtrpcContext, _req: CloseIORequest) -> TtrpcResult<Empty> {
        // TODO call close_io of container
        Ok(Empty::new())
    }

    async fn update(&self, _ctx: &TtrpcContext, mut req: UpdateTaskRequest) -> TtrpcResult<Empty> {
        debug!("Update request for {:?}", req);

        let id = req.take_id();

        let data = req
            .resources
            .into_option()
            .map(|r| r.value)
            .unwrap_or_default();

        let resources: LinuxResources = serde_json::from_slice(&data).map_err(|e| {
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INVALID_ARGUMENT,
                format!("failed to parse resource spec: {}", e),
            ))
        })?;

        let mut container = self.get_container(&id).await?;
        container.update(&resources).await?;
        Ok(Empty::new())
    }

    async fn wait(&self, _ctx: &TtrpcContext, req: WaitRequest) -> TtrpcResult<WaitResponse> {
        info!("Wait request for {:?}", req);
        let exec_id = req.exec_id.as_str().as_option();
        let wait_rx = {
            let mut container = self.get_container(req.id()).await?;
            let state = container.state(exec_id).await?;
            if state.status() != Status::RUNNING && state.status() != Status::CREATED {
                let mut resp = WaitResponse::new();
                resp.exit_status = state.exit_status;
                resp.exited_at = state.exited_at;
                info!("Wait request for {:?} returns {:?}", req, &resp);
                return Ok(resp);
            }
            container.wait_channel(req.exec_id().as_option()).await?
        };

        wait_rx.await.unwrap_or_default();
        // get lock again.
        let container = self.get_container(req.id()).await?;
        let (_, code, exited_at) = container.get_exit_info(exec_id).await?;
        let mut resp = WaitResponse::new();
        resp.set_exit_status(code as u32);
        let ts = convert_to_timestamp(exited_at);
        resp.set_exited_at(ts);
        info!("Wait request for {:?} returns {:?}", req, &resp);
        Ok(resp)
    }

    async fn stats(&self, _ctx: &TtrpcContext, req: StatsRequest) -> TtrpcResult<StatsResponse> {
        debug!("Stats request for {:?}", req);
        let container = self.get_container(req.id()).await?;
        let stats = container.stats().await?;

        let mut resp = StatsResponse::new();
        resp.set_stats(convert_to_any(Box::new(stats))?);
        Ok(resp)
    }

    async fn connect(
        &self,
        _ctx: &TtrpcContext,
        req: ConnectRequest,
    ) -> TtrpcResult<ConnectResponse> {
        info!("Connect request for {:?}", req);
        let container = self.get_container(req.id()).await?;

        Ok(ConnectResponse {
            shim_pid: std::process::id() as u32,
            task_pid: container.pid().await as u32,
            ..Default::default()
        })
    }

    async fn shutdown(&self, _ctx: &TtrpcContext, _req: ShutdownRequest) -> TtrpcResult<Empty> {
        debug!("Shutdown request");
        let containers = self.containers.lock().await;
        if containers.len() > 0 {
            return Ok(Empty::new());
        }
        self.exit.signal();
        Ok(Empty::default())
    }
}
