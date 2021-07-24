use std::collections::hash_map::DefaultHasher;
use std::env;
use std::error;
use std::hash::Hasher;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{self, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time;

pub use containerd_shim_protos as protos;
use protos::protobuf::Message;
use protos::shim::{shim::DeleteResponse, shim_ttrpc::create_task};
use protos::ttrpc::Server;

use thiserror::Error;

mod args;
mod logger;
mod reap;

pub use protos::shim::shim as api;
pub use protos::shim::shim_ttrpc::Task;

pub use protos::ttrpc;
pub use protos::ttrpc::Result as TtrpcResult;
pub use protos::ttrpc::TtrpcContext;

/// Config of shim binary options provided by shim implementations
#[derive(Debug, Default)]
pub struct Config {
    /// Disables automatic configuration of logrus to use the shim FIFO
    pub no_setup_logger: bool,
    /// Disables setting the shim as a child subreaper.
    pub no_sub_reaper: bool,
}

#[derive(Debug, Default)]
pub struct StartOpts {
    /// ID of the container.
    pub id: String,
    /// Binary path to publish events back to containerd.
    pub publish_binary: String,
    /// Address of the containerd's main socket.
    pub address: String,
    /// TTRPC socket address.
    pub ttrpc_address: String,
    /// Namespace for the container.
    pub namespace: String,
}

/// Shim interface that must be implemented by clients.
pub trait Shim: Task {
    fn new(id: &str, namespace: &str, config: &mut Config) -> Self;

    /// Launch new shim.
    /// See https://github.com/containerd/containerd/tree/master/runtime/v2#start
    fn start_shim(&mut self, opts: StartOpts) -> Result<String, Box<dyn error::Error>> {
        let address = spawn(opts)?;
        Ok(address)
    }

    fn delete_shim(&mut self) -> Result<DeleteResponse, Box<dyn error::Error>> {
        Ok(DeleteResponse::default())
    }
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
    // Parse command line
    let os_args: Vec<_> = env::args_os().collect();
    let flags = args::parse(&os_args[1..])?;

    let ttrpc_address = env::var("TTRPC_ADDRESS")?;

    // Create shim instance
    let mut config = Config::default();
    let mut shim = T::new(id, &flags.namespace, &mut config);

    if !config.no_sub_reaper {
        reap::set_subreaper()?;
    }

    match flags.action.as_str() {
        "start" => {
            let args = StartOpts {
                id: id.into(),
                publish_binary: flags.publish_binary,
                address: flags.address,
                ttrpc_address,
                namespace: flags.namespace,
            };

            let address = shim.start_shim(args).map_err(Error::Start)?;
            io::stdout().lock().write_fmt(format_args!("{}", address))?;

            Ok(())
        }
        "delete" => {
            let response = shim.delete_shim().map_err(Error::Cleanup)?;

            let stdout = io::stdout();
            let mut locked = stdout.lock();
            response.write_to_writer(&mut locked)?;

            Ok(())
        }
        _ => {
            if !config.no_setup_logger {
                logger::init(flags.debug)?;
            }

            let task_service = create_task(Arc::new(Box::new(shim)));

            let mut server = Server::new()
                .register_service(task_service)
                .bind(format!("unix://{}", flags.socket).as_str())?;

            server.start()?;

            // TODO: define exit criteria here.
            std::thread::sleep(std::time::Duration::from_secs(360));

            server.shutdown();

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
    Ttrpc(#[from] protos::ttrpc::Error),
    #[error("Protobuf error")]
    Protobuf(#[from] protos::protobuf::error::ProtobufError),
    #[error("Failed to setup logger")]
    Logger(#[from] logger::Error),
    #[error("IO error")]
    Io(#[from] io::Error),
    #[error("Env error")]
    Env(#[from] env::VarError),
    #[error("Failed to start shim")]
    Start(Box<dyn error::Error>),
    #[error("Shim cleanup failed")]
    Cleanup(Box<dyn error::Error>),
}

const SOCKET_ROOT: &str = "/run/containerd";

pub fn socket_address(socket_path: &str, namespace: &str, id: &str) -> String {
    let path = PathBuf::from(socket_path)
        .join(namespace)
        .join(id)
        .display()
        .to_string();

    let hash = {
        let mut hasher = DefaultHasher::new();
        hasher.write(path.as_bytes());
        hasher.finish()
    };

    format!("{}/{:x}.sock", SOCKET_ROOT, hash)
}

/// Spawn is a helper func to launch shim process.
/// Typically this expected to be called from `StartShim`.
pub fn spawn(opts: StartOpts) -> Result<String, Error> {
    let socket_address = socket_address(&opts.address, &opts.namespace, &opts.id);

    Command::new(env::current_exe()?)
        .current_dir(env::current_dir()?)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .args(&[
            "-namespace",
            &opts.namespace,
            "-id",
            &opts.id,
            "-address",
            &opts.address,
            "-socket",
            &socket_address,
        ])
        .spawn()?;

    // This is temp HACK.
    // Give TTRPC server some time to initialize.
    thread::sleep(time::Duration::from_secs(2));

    Ok(socket_address)
}
