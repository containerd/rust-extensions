use containerd_protos::protobuf::Message;
use containerd_protos::shim::{shim::DeleteResponse, shim_ttrpc::create_task};
use containerd_protos::ttrpc::Server;
use std::env;
use std::error;
use std::io::{self, Write};
use std::process;
use std::sync::Arc;
use thiserror::Error;

mod args;

pub use containerd_protos as protos;
pub use containerd_protos::shim::shim_ttrpc::Task;

#[derive(Debug, Default)]
pub struct StartOpts {
    pub id: String,
    pub containerd_binary: String,
    pub address: String,
    pub ttrpc_address: String,
}

pub trait Shim: Task {
    fn new(id: &str, namespace: &str) -> Self;

    fn start_shim(&mut self, opts: StartOpts) -> Result<String, Box<dyn error::Error>>;
    fn cleanup(&mut self) -> Result<DeleteResponse, Box<dyn error::Error>>;
}

pub fn run<T>(id: &str)
where
    T: Shim + Send + Sync + 'static,
{
    if let Some(err) = bootstrap::<T>(id).err() {
        eprintln!("{}: {:?}", id, err);
        process::exit(1);
    }
}

fn bootstrap<T>(id: &str) -> Result<(), Error>
where
    T: Shim + Send + Sync + 'static,
{
    let os_args: Vec<_> = env::args_os().collect();
    let flags = args::parse(&os_args[1..])?;

    let ttrpc_address = env::var("TTRPC_ADDRESS")?;

    let mut shim = T::new(id, &flags.namespace);

    match flags.action.as_str() {
        "start" => {
            let args = StartOpts {
                id: id.into(),
                containerd_binary: flags.publish_binary,
                address: flags.address,
                ttrpc_address,
            };

            let address = shim.start_shim(args).map_err(Error::Start)?;
            io::stdout().lock().write_fmt(format_args!("{}", address))?;

            Ok(())
        }
        "delete" => {
            let response = shim.cleanup().map_err(Error::Cleanup)?;

            let stdout = io::stdout();
            let mut locked = stdout.lock();
            response.write_to_writer(&mut locked)?;

            Ok(())
        }
        _ => {
            let task_service = create_task(Arc::new(Box::new(shim)));

            let host = format!("unix://{}", flags.socket);
            let mut server = Server::new().bind(&host)?.register_service(task_service);

            server.start()?;

            Ok(())
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    /// Invalid command line arguments.
    #[error("Failed to parse command line")]
    Flags(#[from] args::Error),
    /// TTRPC specific error.
    #[error("TTRPC error")]
    Ttrpc(#[from] containerd_protos::ttrpc::Error),
    #[error("Protobuf error")]
    Protobuf(#[from] containerd_protos::protobuf::error::ProtobufError),
    #[error("IO error")]
    Io(#[from] io::Error),
    #[error("Env error")]
    Env(#[from] env::VarError),
    #[error("Failed to start shim")]
    Start(Box<dyn error::Error>),
    #[error("Shim cleanup failed")]
    Cleanup(Box<dyn error::Error>),
}
