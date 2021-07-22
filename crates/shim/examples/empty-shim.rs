use std::error::Error;

use containerd_shim as shim;
use shim::protos;

use shim::api::DeleteResponse;
use shim::StartOpts;

use log::debug;

struct Service;

impl shim::Shim for Service {
    fn new(_id: &str, _namespace: &str, _config: &mut shim::Config) -> Self {
        todo!()
    }

    fn start_shim(&mut self, _opts: StartOpts) -> Result<String, Box<dyn Error>> {
        todo!()
    }

    fn cleanup(&mut self) -> Result<DeleteResponse, Box<dyn Error>> {
        todo!()
    }
}

impl shim::Task for Service {
    fn state(
        &self,
        _ctx: &shim::Context,
        _request: shim::api::StateRequest,
    ) -> protos::ttrpc::Result<shim::api::StateResponse> {
        debug!("Get state");
        Ok(shim::api::StateResponse::default())
    }
}

fn main() {
    shim::run::<Service>("io.containerd.empty.v1")
}
