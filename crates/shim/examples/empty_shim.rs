use containerd_shim as shim;
use shim::{api, TtrpcContext, TtrpcResult};

use log::info;

struct Service;

impl shim::Shim for Service {
    fn new(
        _id: &str,
        _namespace: &str,
        _publisher: shim::RemotePublisher,
        _config: &mut shim::Config,
    ) -> Self {
        Service {}
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
}

fn main() {
    shim::run::<Service>("io.containerd.empty.v1")
}
