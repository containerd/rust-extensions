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

//! A library to implement custom runtime v2 shims for containerd.

use std::collections::hash_map::DefaultHasher;
use std::env;
use std::error;
use std::fs;
use std::hash::Hasher;
use std::io::{self, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};

pub use containerd_shim_client as protos;

use protos::protobuf::Message;
use protos::shim::{shim::DeleteResponse, shim_ttrpc::create_task};
use protos::ttrpc::Server;

use command_fds::{CommandFdExt, FdMapping};
use log::info;
use nix::errno::Errno;
use thiserror::Error;

mod args;
mod logger;
mod publisher;
mod reap;

pub use publisher::RemotePublisher;

/// Generated request/response structures.
pub mod api {
    pub use super::protos::shim::empty::Empty;
    pub use super::protos::shim::shim::*;
}

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

/// Startup options received from containerd to start new shim instance.
///
/// These will be passed via [`Shim::start_shim`] to shim.
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

/// Helper structure that wraps atomic bool to signal shim server when to shutdown the TTRPC server.
///
/// Shim implementations are responsible for calling [`Self::signal`].
#[derive(Clone)]
pub struct ExitSignal(Arc<(Mutex<bool>, Condvar)>);

impl Default for ExitSignal {
    #[allow(clippy::mutex_atomic)]
    fn default() -> Self {
        ExitSignal(Arc::new((Mutex::new(false), Condvar::new())))
    }
}

impl ExitSignal {
    /// Set exit signal to shutdown shim server.
    pub fn signal(&self) {
        let (lock, cvar) = &*self.0;
        let mut exit = lock.lock().unwrap();
        *exit = true;
        cvar.notify_all();
    }

    /// Wait for the exit signal to be set.
    pub fn wait(&self) {
        let (lock, cvar) = &*self.0;
        let mut started = lock.lock().unwrap();
        while !*started {
            started = cvar.wait(started).unwrap();
        }
    }
}

/// Main shim interface that must be implemented by all shims.
///
/// Start and delete routines will be called to handle containerd's shim lifecycle requests.
pub trait Shim {
    /// Error type to be returned when starting/deleting shim.
    type Error: error::Error;

    /// Type to provide task service for the shim.
    type T: Task + Send + Sync;

    /// Create a new instance of Shim.
    fn new(id: &str, namespace: &str, publisher: RemotePublisher, config: &mut Config) -> Self;

    /// Start shim will be called by containerd when launching new shim instance.
    ///
    /// It expected to return TTRPC address containerd daemon can use to communicate with
    /// the given shim instance.
    /// See https://github.com/containerd/containerd/tree/master/runtime/v2#start
    fn start_shim(&mut self, opts: StartOpts) -> Result<String, Self::Error>;

    /// Delete shim will be called by containerd after shim shutdown to cleanup any leftovers.
    fn delete_shim(&mut self) -> Result<DeleteResponse, Self::Error> {
        Ok(DeleteResponse::default())
    }

    /// Wait for the shim to exit.
    fn wait(&mut self);

    /// Get the task service object.
    fn get_task_service(&self) -> Self::T;
}

/// Shim entry point that must be invoked from `main`.
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
    let publisher = publisher::RemotePublisher::new(&ttrpc_address)?;

    // Create shim instance
    let mut config = Config::default();
    let mut shim = T::new(id, &flags.namespace, publisher, &mut config);

    if !config.no_sub_reaper {
        reap::set_subreaper()?;
    }

    match flags.action.as_str() {
        "start" => {
            let args = StartOpts {
                id: flags.id,
                publish_binary: flags.publish_binary,
                address: flags.address,
                ttrpc_address,
                namespace: flags.namespace,
            };

            let address = shim
                .start_shim(args)
                .map_err(|err| Error::Start(err.to_string()))?;

            io::stdout().lock().write_fmt(format_args!("{}", address))?;

            Ok(())
        }
        "delete" => {
            let response = shim
                .delete_shim()
                .map_err(|err| Error::Delete(err.to_string()))?;

            let stdout = io::stdout();
            let mut locked = stdout.lock();
            response.write_to_writer(&mut locked)?;

            Ok(())
        }
        _ => {
            if !config.no_setup_logger {
                logger::init(flags.debug)?;
            }

            let task = shim.get_task_service();
            let task_service = create_task(Arc::new(Box::new(task)));
            let mut server = Server::new().register_service(task_service);

            server = if flags.socket.is_empty() {
                server.add_listener(SOCKET_FD)?
            } else {
                server.bind(&flags.socket)?
            };

            server.start()?;

            info!("Shim successfully started, waiting for exit signal...");
            shim.wait();

            info!("Shutting down shim instance");
            server.shutdown();

            Ok(())
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    /// Invalid command line arguments.
    #[error("Failed to parse command line: {0}")]
    Flags(#[from] args::Error),

    /// TTRPC specific error.
    #[error("TTRPC error: {0}")]
    Ttrpc(#[from] protos::ttrpc::Error),

    #[error("Protobuf error: {0}")]
    Protobuf(#[from] protos::protobuf::error::ProtobufError),

    #[error("Failed to setup logger: {0}")]
    Logger(#[from] logger::Error),

    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Env error: {0}")]
    Env(#[from] env::VarError),

    #[error("Failed to start shim: {0}")]
    Start(String),

    #[error("Failed to delete shim: {0}")]
    Delete(String),

    #[error("Publisher error: {0}")]
    Publisher(#[from] publisher::Error),

    /// Unable to pass fd to child process (we rely on `command_fds` crate for this).
    #[error("Failed to pass socket fd to child: {0}")]
    FdMap(#[from] command_fds::FdMappingCollision),

    #[error("Error code: {0}")]
    Errno(#[from] Errno),
}

const SOCKET_FD: RawFd = 3;

#[cfg(target_os = "linux")]
pub const SOCKET_ROOT: &str = "/run/containerd";

#[cfg(target_os = "macos")]
pub const SOCKET_ROOT: &str = "/var/run/containerd";

/// Make socket path from containerd socket path, namespace and id.
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

fn start_listener(address: &str) -> Result<UnixListener, Error> {
    // Try to create the needed directory hierarchy.
    if let Some(parent) = Path::new(address).parent() {
        fs::create_dir_all(parent)?;
    }

    UnixListener::bind(address).or_else(|e| {
        if e.kind() == io::ErrorKind::AddrInUse {
            if let Ok(md) = Path::new(address).metadata() {
                if md.file_type().is_socket() {
                    fs::remove_file(address)?;
                    return UnixListener::bind(address).map_err(|e| e.into());
                }
            }
        }
        Err(e.into())
    })
}

/// Spawn is a helper func to launch shim process.
/// Typically this expected to be called from `StartShim`.
pub fn spawn(opts: StartOpts, vars: Vec<(&str, &str)>) -> Result<String, Error> {
    let cmd = env::current_exe()?;
    let cwd = env::current_dir()?;
    let address = socket_address(&opts.address, &opts.namespace, &opts.id);

    // Create socket and prepare listener.
    // We'll use `add_listener` when creating TTRPC server.
    let listener = start_listener(&address)?;
    let mut command = Command::new(cmd);

    command
        .current_dir(cwd)
        .stdout(Stdio::null())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .fd_mappings(vec![FdMapping {
            parent_fd: listener.as_raw_fd(),
            child_fd: SOCKET_FD,
        }])?
        .args(&[
            "-namespace",
            &opts.namespace,
            "-id",
            &opts.id,
            "-address",
            &opts.address,
        ])
        .envs(vars);

    command.spawn().map_err(Into::into).map(|_| {
        // Ownership of `listener` has been passed to child.
        std::mem::forget(listener);
        format!("unix://{}", address)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn exit_signal() {
        let signal = ExitSignal::default();

        let cloned = signal.clone();
        let handle = thread::spawn(move || {
            cloned.signal();
        });

        signal.wait();

        if let Err(err) = handle.join() {
            panic!("{:?}", err);
        }
    }

    #[test]
    fn test_start_listener() {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().to_str().unwrap().to_owned();

        // A little dangerous, may be turned on under controlled environment.
        //assert!(start_listener("/").is_err());
        //assert!(start_listener("/tmp").is_err());

        let socket = path + "/ns1/id1/socket";
        let _listener = start_listener(&socket).unwrap();
        let _listener2 = start_listener(&socket).unwrap();

        let socket2 = socket + "/socket";
        assert!(start_listener(&socket2).is_err());

        let path = tmpdir.path().to_str().unwrap().to_owned();
        let txt_file = path + "demo.txt";
        fs::write(&txt_file, "test").unwrap();
        assert!(start_listener(&txt_file).is_err());
        let context = fs::read_to_string(&txt_file).unwrap();
        assert_eq!(context, "test");
    }
}
