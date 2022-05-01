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

use std::convert::TryFrom;
use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::{self, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};

use command_fds::{CommandFdExt, FdMapping};
use libc::{SIGCHLD, SIGINT, SIGPIPE, SIGTERM};
pub use log::{debug, error, info, warn};
use nix::errno::Errno;
use nix::sys::signal::Signal;
use nix::sys::wait::{self, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use signal_hook::iterator::Signals;

use crate::protos::protobuf::Message;
use crate::protos::shim::shim_ttrpc::{create_task, Task};
use crate::protos::ttrpc::{Client, Server};
use util::{read_address, write_address};

use crate::api::DeleteResponse;
use crate::synchronous::monitor::monitor_notify_by_pid;
use crate::synchronous::publisher::RemotePublisher;
use crate::Error;
use crate::{args, logger, reap, Result, TTRPC_ADDRESS};
use crate::{parse_sockaddr, socket_address, start_listener, Config, StartOpts, SOCKET_FD};

pub mod monitor;
pub mod publisher;
pub mod util;

pub mod console;

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
    /// - `config`: for the shim to pass back configuration information
    fn new(runtime_id: &str, id: &str, namespace: &str, config: &mut Config) -> Self;

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
    fn create_task_service(&self, publisher: RemotePublisher) -> Self::T;
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

    // Create shim instance
    let mut config = opts.unwrap_or_default();

    // Setup signals
    let signals = setup_signals(&config);

    if !config.no_sub_reaper {
        reap::set_subreaper()?;
    }

    let mut shim = T::new(runtime_id, &flags.id, &flags.namespace, &mut config);

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

            let publisher = publisher::RemotePublisher::new(&ttrpc_address)?;
            let task = shim.create_task_service(publisher);
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
                }
                SIGCHLD => loop {
                    // Note that this thread sticks to child even it is suspended.
                    match wait::waitpid(Some(Pid::from_raw(-1)), Some(WaitPidFlag::WNOHANG)) {
                        Ok(WaitStatus::Exited(pid, status)) => {
                            monitor_notify_by_pid(pid.as_raw(), status)
                                .unwrap_or_else(|e| error!("failed to send exit event {}", e))
                        }
                        Ok(WaitStatus::Signaled(pid, sig, _)) => {
                            debug!("child {} terminated({})", pid, sig);
                            let exit_code = 128 + sig as i32;
                            monitor_notify_by_pid(pid.as_raw(), exit_code)
                                .unwrap_or_else(|e| error!("failed to send signal event {}", e))
                        }
                        Err(Errno::ECHILD) => {
                            break;
                        }
                        Err(e) => {
                            // stick until all children will be successfully waited, even some unexpected error occurs.
                            warn!("error occurred in signal handler: {}", e);
                        }
                        _ => {} // stick until exit
                    }
                },
                _ => {
                    if let Ok(sig) = Signal::try_from(sig) {
                        debug!("received {}", sig);
                    } else {
                        warn!("received invalid signal {}", sig);
                    }
                }
            }
        }
    }
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
                return Err(Error::IoError {
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
}
