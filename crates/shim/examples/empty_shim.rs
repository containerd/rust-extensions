use containerd_shim as shim;

use log::info;
use shim::{api, TtrpcContext, TtrpcResult};

struct Service {
    exit: shim::ExitSignal,
}

impl shim::Shim for Service {
    type Error = shim::Error;

    fn new(
        _id: &str,
        _namespace: &str,
        _publisher: shim::RemotePublisher,
        _config: &mut shim::Config,
        exit: shim::ExitSignal,
    ) -> Self {
        Service { exit }
    }

    fn start_shim(&mut self, opts: shim::StartOpts) -> Result<String, shim::Error> {
        let address = shim::spawn(opts)?;
        Ok(address)
    }
}

impl shim::Task for Service {
    fn connect(
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

    fn shutdown(&self, _ctx: &TtrpcContext, _req: api::ShutdownRequest) -> TtrpcResult<api::Empty> {
        info!("Shutdown request");
        self.exit.signal();
        Ok(api::Empty::default())
    }
}

fn main() {
    shim::run::<Service>("io.containerd.empty.v1")
}
