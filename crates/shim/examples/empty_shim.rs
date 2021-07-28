use containerd_shim as shim;

use log::info;
use shim::{api, TtrpcContext, TtrpcResult};
use std::error::Error;

struct Service {
    exit: shim::ExitSignal,
}

impl shim::Shim for Service {
    fn new(
        _id: &str,
        _namespace: &str,
        _publisher: shim::RemotePublisher,
        _config: &mut shim::Config,
        exit: shim::ExitSignal,
    ) -> Self {
        Service { exit }
    }

    fn start_shim(&mut self, _opts: shim::StartOpts) -> Result<String, Box<dyn Error>> {
        Ok("Socket address here".into())
    }
}

impl shim::Task for Service {
    fn create(
        &self,
        _ctx: &TtrpcContext,
        _req: api::CreateTaskRequest,
    ) -> TtrpcResult<api::CreateTaskResponse> {
        info!("Create");
        Ok(api::CreateTaskResponse::default())
    }

    fn shutdown(&self, _ctx: &TtrpcContext, _req: api::ShutdownRequest) -> TtrpcResult<api::Empty> {
        self.exit.signal();
        Ok(api::Empty::default())
    }
}

fn main() {
    shim::run::<Service>("io.containerd.empty.v1")
}
