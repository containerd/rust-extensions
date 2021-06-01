use containerd_protos::shim::shim::DeleteResponse;
use shim::StartOpts;
use std::error::Error;

struct Service;

impl shim::Shim for Service {
    fn new(id: &str, namespace: &str) -> Self {
        todo!()
    }

    fn start_shim(&mut self, opts: StartOpts) -> Result<String, Box<dyn Error>> {
        todo!()
    }

    fn cleanup(&mut self) -> Result<DeleteResponse, Box<dyn Error>> {
        todo!()
    }
}

impl shim::Task for Service {}

fn main() {
    shim::run::<Service>("io.containerd.empty.v1")
}
