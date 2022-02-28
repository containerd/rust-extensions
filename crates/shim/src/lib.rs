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
//!
//! # Runtime
//! Runtime v2 introduces a first class shim API for runtime authors to integrate with containerd.
//! The shim API is minimal and scoped to the execution lifecycle of a container.
//!
//! This crate simplifies shim v2 runtime development for containerd. It handles common tasks such
//! as command line parsing, setting up shim's TTRPC server, logging, events, etc.
//!
//! Clients are expected to implement [Shim] and [Task] traits with task handling routines.
//! This generally replicates same API as in Go [version](https://github.com/containerd/containerd/blob/main/runtime/v2/example/cmd/main.go).
//!
//! Once implemented, shim's bootstrap code is as easy as:
//! ```text
//! shim::run::<Service>("io.containerd.empty.v1")
//! ```
//!

use std::collections::hash_map::DefaultHasher;
use std::env;
use std::fs;
use std::hash::Hasher;
use std::io::Write;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};

use command_fds::{CommandFdExt, FdMapping};
use libc::{c_int, pid_t, SIGCHLD, SIGINT, SIGPIPE, SIGTERM};
pub use log::{debug, error, info, warn};
use signal_hook::iterator::Signals;

pub use containerd_shim_protos as protos;
use protos::protobuf::Message;
pub use protos::shim::shim::DeleteResponse;
pub use protos::shim::shim_ttrpc::{create_task, Task};
pub use protos::ttrpc::{context::Context, Result as TtrpcResult, TtrpcContext};
use protos::ttrpc::{Client, Server};

pub use crate::error::{Error, Result};
use crate::monitor::monitor_notify_by_pid;
pub use crate::publisher::RemotePublisher;
use crate::util::{read_address, write_address};

#[macro_use]
pub mod error;

/// Generated request/response structures.
pub mod api {
    pub use super::protos::api::Status;
    pub use super::protos::shim::oci::Options;
    pub use super::protos::shim::shim::*;
    pub use super::protos::types::empty::Empty;
}
mod args;
mod logger;
pub mod monitor;
pub mod mount;
mod publisher;
mod reap;
pub mod util;

#[cfg(feature = "async")]
pub mod asynchronous;
pub mod console;
pub mod io;

const TTRPC_ADDRESS: &str = "TTRPC_ADDRESS";

/// Config of shim binary options provided by shim implementations
#[derive(Debug, Default)]
pub struct Config {
    /// Disables automatic configuration of logrus to use the shim FIFO
    pub no_setup_logger: bool,
    /// Disables the shim binary from reaping any child process implicitly
    pub no_reaper: bool,
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

    pub debug: bool,
}

/// Helper structure that wraps atomic bool to signal shim server when to shutdown the TTRPC server.
///
/// Shim implementations are responsible for calling [`Self::signal`].
#[allow(clippy::mutex_atomic)] // Condvar expected to be used with Mutex, not AtomicBool.
#[derive(Default)]
pub struct ExitSignal(Mutex<bool>, Condvar);

#[allow(clippy::mutex_atomic)]
impl ExitSignal {
    /// Set exit signal to shutdown shim server.
    pub fn signal(&self) {
        let (lock, cvar) = (&self.0, &self.1);
        let mut exit = lock.lock().unwrap();
        *exit = true;
        cvar.notify_all();
    }

    /// Wait for the exit signal to be set.
    pub fn wait(&self) {
        let (lock, cvar) = (&self.0, &self.1);
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
    /// Type to provide task service for the shim.
    type T: Task + Send + Sync;

    /// Create a new instance of Shim.
    ///
    /// # Arguments
    /// - `runtime_id`: identifier of the container runtime.
    /// - `id`: identifier of the shim/container, passed in from Containerd.
    /// - `namespace`: namespace of the shim/container, passed in from Containerd.
    /// - `publisher`: publisher to send events to Containerd.
    /// - `config`: for the shim to pass back configuration information
    fn new(
        runtime_id: &str,
        id: &str,
        namespace: &str,
        publisher: RemotePublisher,
        config: &mut Config,
    ) -> Self;

    /// Start shim will be called by containerd when launching new shim instance.
    ///
    /// It expected to return TTRPC address containerd daemon can use to communicate with
    /// the given shim instance.
    ///
    /// See https://github.com/containerd/containerd/tree/master/runtime/v2#start
    fn start_shim(&mut self, opts: StartOpts) -> Result<String>;

    /// Delete shim will be called by containerd after shim shutdown to cleanup any leftovers.
    fn delete_shim(&mut self) -> Result<DeleteResponse>;

    /// Wait for the shim to exit.
    fn wait(&mut self);

    /// Create the task service object.
    fn create_task_service(&self) -> Self::T;
}

/// Shim entry point that must be invoked from `main`.
pub fn run<T>(runtime_id: &str, opts: Option<Config>)
where
    T: Shim + Send + Sync + 'static,
{
    if let Some(err) = bootstrap::<T>(runtime_id, opts).err() {
        eprintln!("{}: {:?}", runtime_id, err);
        process::exit(1);
    }
}

fn bootstrap<T>(runtime_id: &str, opts: Option<Config>) -> Result<()>
where
    T: Shim + Send + Sync + 'static,
{
    // Parse command line
    let os_args: Vec<_> = env::args_os().collect();
    let flags = args::parse(&os_args[1..])?;

    let ttrpc_address = env::var(TTRPC_ADDRESS)?;
    let publisher = publisher::RemotePublisher::new(&ttrpc_address)?;

    // Create shim instance
    let mut config = opts.unwrap_or_else(Config::default);

    // Setup signals
    let signals = setup_signals(&config);

    if !config.no_sub_reaper {
        reap::set_subreaper()?;
    }

    let mut shim = T::new(
        runtime_id,
        &flags.id,
        &flags.namespace,
        publisher,
        &mut config,
    );

    match flags.action.as_str() {
        "start" => {
            let args = StartOpts {
                id: flags.id,
                publish_binary: flags.publish_binary,
                address: flags.address,
                ttrpc_address,
                namespace: flags.namespace,
                debug: flags.debug,
            };

            let address = shim.start_shim(args)?;

            std::io::stdout()
                .lock()
                .write_fmt(format_args!("{}", address))
                .map_err(io_error!(e, "write stdout"))?;

            Ok(())
        }
        "delete" => {
            std::thread::spawn(move || handle_signals(signals));
            let response = shim.delete_shim()?;
            let stdout = std::io::stdout();
            let mut locked = stdout.lock();
            response.write_to_writer(&mut locked)?;

            Ok(())
        }
        _ => {
            if !config.no_setup_logger {
                logger::init(flags.debug)?;
            }

            let task = shim.create_task_service();
            let task_service = create_task(Arc::new(Box::new(task)));
            let mut server = Server::new().register_service(task_service);
            server = server.add_listener(SOCKET_FD)?;
            server.start()?;

            info!("Shim successfully started, waiting for exit signal...");
            std::thread::spawn(move || handle_signals(signals));
            shim.wait();

            info!("Shutting down shim instance");
            server.shutdown();

            // NOTE: If the shim server is down(like oom killer), the address
            // socket might be leaking.
            let address = read_address()?;
            remove_socket_silently(&address);
            Ok(())
        }
    }
}

fn setup_signals(config: &Config) -> Signals {
    let signals = Signals::new(&[SIGTERM, SIGINT, SIGPIPE]).expect("new signal failed");
    if !config.no_reaper {
        signals.add_signal(SIGCHLD).expect("add signal failed");
    }
    signals
}

fn handle_signals(mut signals: Signals) {
    loop {
        for sig in signals.wait() {
            match sig {
                SIGTERM | SIGINT => {
                    debug!("received {}", sig);
                    return;
                }
                SIGCHLD => loop {
                    unsafe {
                        let pid: pid_t = -1;
                        let mut status: c_int = 0;
                        let options: c_int = libc::WNOHANG;
                        let res_pid = libc::waitpid(pid, &mut status, options);
                        let status = libc::WEXITSTATUS(status);
                        if res_pid <= 0 {
                            break;
                        } else {
                            monitor_notify_by_pid(res_pid, status).unwrap_or_else(|e| {
                                error!("failed to send exit event {}", e);
                            });
                        }
                    }
                },
                _ => {
                    debug!("received {}", sig);
                }
            }
        }
    }
}

/// The shim process communicates with the containerd server through a communication channel
/// created by containerd. One endpoint of the communication channel is passed to shim process
/// through a file descriptor during forking, which is the fourth(3) file descriptor.
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

    format!("unix://{}/{:x}.sock", SOCKET_ROOT, hash)
}

fn parse_sockaddr(addr: &str) -> &str {
    if let Some(addr) = addr.strip_prefix("unix://") {
        return addr;
    }

    if let Some(addr) = addr.strip_prefix("vsock://") {
        return addr;
    }

    addr
}

fn start_listener(address: &str) -> std::io::Result<UnixListener> {
    let path = parse_sockaddr(address);
    // Try to create the needed directory hierarchy.
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent)?;
    }
    UnixListener::bind(path)
}

fn wait_socket_working(address: &str, interval_in_ms: u64, count: u32) -> Result<()> {
    for _i in 0..count {
        match Client::connect(address) {
            Ok(_) => {
                return Ok(());
            }
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(interval_in_ms));
            }
        }
    }
    Err(other!("time out waiting for socket {}", address))
}

fn remove_socket_silently(address: &str) {
    remove_socket(address).unwrap_or_else(|e| warn!("failed to remove file {} {:?}", address, e))
}

fn remove_socket(address: &str) -> Result<()> {
    let path = parse_sockaddr(address);
    if let Ok(md) = Path::new(path).metadata() {
        if md.file_type().is_socket() {
            fs::remove_file(path).map_err(io_error!(e, "remove socket"))?;
        }
    }
    Ok(())
}

/// Spawn is a helper func to launch shim process.
/// Typically this expected to be called from `StartShim`.
pub fn spawn(opts: StartOpts, grouping: &str, vars: Vec<(&str, &str)>) -> Result<(u32, String)> {
    let cmd = env::current_exe().map_err(io_error!(e, ""))?;
    let cwd = env::current_dir().map_err(io_error!(e, ""))?;
    let address = socket_address(&opts.address, &opts.namespace, grouping);

    // Create socket and prepare listener.
    // We'll use `add_listener` when creating TTRPC server.
    let listener = match start_listener(&address) {
        Ok(l) => l,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::AddrInUse {
                return Err(error::Error::IoError {
                    context: "".to_string(),
                    err: e,
                });
            };
            if let Ok(()) = wait_socket_working(&address, 5, 200) {
                write_address(&address)?;
                return Ok((0, address));
            }
            remove_socket(&address)?;
            start_listener(&address).map_err(io_error!(e, ""))?
        }
    };

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
        ]);
    if opts.debug {
        command.arg("-debug");
    }
    command.envs(vars);

    command
        .spawn()
        .map_err(io_error!(e, "spawn shim"))
        .map(|child| {
            // Ownership of `listener` has been passed to child.
            std::mem::forget(listener);
            (child.id(), address)
        })
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn exit_signal() {
        let signal = Arc::new(ExitSignal::default());

        let cloned = Arc::clone(&signal);
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
        let _listener2 = start_listener(&socket).expect_err("socket should already in use");

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
