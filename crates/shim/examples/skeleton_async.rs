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

use std::sync::Arc;

use async_trait::async_trait;
use log::info;

use containerd_shim::asynchronous::{run, spawn, ExitSignal, Shim};
use containerd_shim::publisher::RemotePublisher;
use containerd_shim::{Config, Error, StartOpts, TtrpcResult};
use containerd_shim_protos::api;
use containerd_shim_protos::api::DeleteResponse;
use containerd_shim_protos::shim_async::Task;
use containerd_shim_protos::ttrpc::r#async::TtrpcContext;

#[derive(Clone)]
struct Service {
    exit: Arc<ExitSignal>,
}

#[async_trait]
impl Shim for Service {
    type T = Service;

    async fn new(_runtime_id: &str, _id: &str, _namespace: &str, _config: &mut Config) -> Self {
        Service {
            exit: Arc::new(ExitSignal::default()),
        }
    }

    async fn start_shim(&mut self, opts: StartOpts) -> Result<String, Error> {
        let grouping = opts.id.clone();
        let address = spawn(opts, &grouping, Vec::new()).await?;
        Ok(address)
    }

    async fn delete_shim(&mut self) -> Result<DeleteResponse, Error> {
        Ok(DeleteResponse::new())
    }

    async fn wait(&mut self) {
        self.exit.wait().await;
    }

    async fn create_task_service(&self, _publisher: RemotePublisher) -> Self::T {
        self.clone()
    }
}

#[async_trait]
impl Task for Service {
    async fn connect(
        &self,
        _ctx: &TtrpcContext,
        _req: api::ConnectRequest,
    ) -> TtrpcResult<api::ConnectResponse> {
        info!("Connect request");
        Ok(api::ConnectResponse {
            version: String::from("example"),
            ..Default::default()
        })
    }

    async fn shutdown(
        &self,
        _ctx: &TtrpcContext,
        _req: api::ShutdownRequest,
    ) -> TtrpcResult<api::Empty> {
        info!("Shutdown request");
        self.exit.signal();
        Ok(api::Empty::default())
    }
}

#[tokio::main]
async fn main() {
    run::<Service>("io.containerd.empty.v1", None).await;
}
