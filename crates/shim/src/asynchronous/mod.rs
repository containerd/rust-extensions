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

use std::os::unix::fs::FileTypeExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::{env, process};

use async_trait::async_trait;
use command_fds::{CommandFdExt, FdMapping};
use futures::StreamExt;
use libc::{c_int, pid_t, SIGCHLD, SIGINT, SIGPIPE, SIGTERM};
use log::{debug, error, info, warn};
use signal_hook_tokio::Signals;
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;

use containerd_shim_protos::api::DeleteResponse;
use containerd_shim_protos::protobuf::Message;
use containerd_shim_protos::shim_async::{create_task, Client, Task};
use containerd_shim_protos::ttrpc::r#async::Server;

use crate::asynchronous::monitor::monitor_notify_by_pid;
use crate::asynchronous::publisher::RemotePublisher;
use crate::error::Error;
use crate::error::Result;
use crate::util::{asyncify, read_file_to_str, write_str_to_file};
use crate::{
    args, logger, parse_sockaddr, reap, socket_address, Config, StartOpts, SOCKET_FD, TTRPC_ADDRESS,
};

pub mod console;
pub mod container;
pub mod monitor;
pub mod processes;
pub mod publisher;
pub mod task;
pub mod util;

/// Asynchronous Main shim interface that must be implemented by all async shims.
///
/// Start and delete routines will be called to handle containerd's shim lifecycle requests.
#[async_trait]
pub trait Shim {
    /// Type to provide task service for the shim.
    type T: Task + Send + Sync;

    /// Create a new instance of  async Shim.
    ///
    /// # Arguments
    /// - `runtime_id`: identifier of the container runtime.
    /// - `id`: identifier of the shim/container, passed in from Containerd.
    /// - `namespace`: namespace of the shim/container, passed in from Containerd.
    /// - `config`: for the shim to pass back configuration information
    async fn new(runtime_id: &str, id: &str, namespace: &str, config: &mut Config) -> Self;

    /// Start shim will be called by containerd when launching new shim instance.
    ///
    /// It expected to return TTRPC address containerd daemon can use to communicate with
    /// the given shim instance.
    /// See https://github.com/containerd/containerd/tree/master/runtime/v2#start
    /// this is an asynchronous call
    async fn start_shim(&mut self, opts: StartOpts) -> Result<String>;

    /// Delete shim will be called by containerd after shim shutdown to cleanup any leftovers.
    /// this is an asynchronous call
    async fn delete_shim(&mut self) -> Result<DeleteResponse>;

    /// Wait for the shim to exit asynchronously.
    async fn wait(&mut self);

    /// Create the task service object asynchronously.
    async fn create_task_service(&self, publisher: RemotePublisher) -> Self::T;
}

/// Async Shim entry point that must be invoked from tokio `main`.
pub async fn run<T>(runtime_id: &str, opts: Option<Config>)
where
    T: Shim + Send + Sync + 'static,
{
    if let Some(err) = bootstrap::<T>(runtime_id, opts).await.err() {
        eprintln!("{}: {:?}", runtime_id, err);
        process::exit(1);
    }
}

async fn bootstrap<T>(runtime_id: &str, opts: Option<Config>) -> Result<()>
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
    let signals = setup_signals_tokio(&config);

    if !config.no_sub_reaper {
        reap::set_subreaper()?;
    }

    let mut shim = T::new(runtime_id, &flags.id, &flags.namespace, &mut config).await;

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

            let address = shim.start_shim(args).await?;
            let mut stdout = tokio::io::stdout();
            stdout
                .write_all(address.as_bytes())
                .await
                .map_err(io_error!(e, "write stdout"))?;
            // containerd occasionally read an empty string without flushing the stdout
            stdout.flush().await.map_err(io_error!(e, "flush stdout"))?;
            Ok(())
        }
        "delete" => {
            tokio::spawn(async move {
                handle_signals(signals).await;
            });
            let response = shim.delete_shim().await?;
            let resp_bytes = response.write_to_bytes()?;
            tokio::io::stdout()
                .write_all(resp_bytes.as_slice())
                .await
                .map_err(io_error!(e, "failed to write response"))?;

            Ok(())
        }
        _ => {
            if !config.no_setup_logger {
                logger::init(flags.debug)?;
            }

            let publisher = RemotePublisher::new(&ttrpc_address).await?;
            let task = shim.create_task_service(publisher).await;
            let task_service = create_task(Arc::new(Box::new(task)));
            let mut server = Server::new().register_service(task_service);
            server = server.add_listener(SOCKET_FD)?;
            server = server.set_domain_unix();
            server.start().await?;

            info!("Shim successfully started, waiting for exit signal...");
            tokio::spawn(async move {
                handle_signals(signals).await;
            });
            shim.wait().await;

            info!("Shutting down shim instance");
            server.shutdown().await.unwrap_or_default();

            // NOTE: If the shim server is down(like oom killer), the address
            // socket might be leaking.
            if let Ok(address) = read_file_to_str("address").await {
                remove_socket_silently(&address).await;
            }
            Ok(())
        }
    }
}

/// Helper structure that wraps atomic bool to signal shim server when to shutdown the TTRPC server.
///
/// Shim implementations are responsible for calling [`Self::signal`].
pub struct ExitSignal {
    notifier: Notify,
    exited: AtomicBool,
}

impl Default for ExitSignal {
    fn default() -> Self {
        ExitSignal {
            notifier: Notify::new(),
            exited: AtomicBool::new(false),
        }
    }
}

impl ExitSignal {
    /// Set exit signal to shutdown shim server.
    pub fn signal(&self) {
        self.exited.store(true, Ordering::SeqCst);
        self.notifier.notify_waiters();
    }

    /// Wait for the exit signal to be set.
    pub async fn wait(&self) {
        loop {
            let notified = self.notifier.notified();
            if self.exited.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

/// Spawn is a helper func to launch shim process asynchronously.
/// Typically this expected to be called from `StartShim`.
pub async fn spawn(opts: StartOpts, grouping: &str, vars: Vec<(&str, &str)>) -> Result<String> {
    let cmd = env::current_exe().map_err(io_error!(e, ""))?;
    let cwd = env::current_dir().map_err(io_error!(e, ""))?;
    let address = socket_address(&opts.address, &opts.namespace, grouping);

    // Create socket and prepare listener.
    // We'll use `add_listener` when creating TTRPC server.
    let listener = match start_listener(&address).await {
        Ok(l) => l,
        Err(e) => {
            if let Error::IoError {
                err: ref io_err, ..
            } = e
            {
                if io_err.kind() != std::io::ErrorKind::AddrInUse {
                    return Err(e);
                };
            }
            if let Ok(()) = wait_socket_working(&address, 5, 200).await {
                write_str_to_file("address", &address).await?;
                return Ok(address);
            }
            remove_socket(&address).await?;
            start_listener(&address).await?
        }
    };

    // tokio::process::Command do not have method `fd_mappings`,
    // and the `spawn()` is also not an async method,
    // so we use the std::process::Command here
    let mut command = Command::new(cmd);

    command
        .current_dir(cwd)
        .stdout(Stdio::null())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .args(&[
            "-namespace",
            &opts.namespace,
            "-id",
            &opts.id,
            "-address",
            &opts.address,
        ])
        .fd_mappings(vec![FdMapping {
            parent_fd: listener.as_raw_fd(),
            child_fd: SOCKET_FD,
        }])?;
    if opts.debug {
        command.arg("-debug");
    }
    command.envs(vars);

    let _child = command.spawn().map_err(io_error!(e, "spawn shim"))?;
    #[cfg(target_os = "linux")]
    crate::cgroup::set_cgroup_and_oom_score(_child.id())?;
    std::mem::forget(listener);
    Ok(address)
}

fn setup_signals_tokio(config: &Config) -> Signals {
    if config.no_reaper {
        Signals::new(&[SIGTERM, SIGINT, SIGPIPE]).expect("new signal failed")
    } else {
        Signals::new(&[SIGTERM, SIGINT, SIGPIPE, SIGCHLD]).expect("new signal failed")
    }
}

async fn handle_signals(signals: Signals) {
    let mut signals = signals.fuse();
    while let Some(sig) = signals.next().await {
        match sig {
            SIGTERM | SIGINT => {
                debug!("received {}", sig);
            }
            SIGCHLD => loop {
                let result = asyncify(move || -> Result<(pid_t, c_int)> {
                    let mut status: c_int = -1;
                    let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
                    if pid <= 0 {
                        return Err(other!("wait finished"));
                    }
                    let status = libc::WEXITSTATUS(status);
                    Ok((pid, status))
                })
                .await;

                if let Ok((res_pid, status)) = result {
                    monitor_notify_by_pid(res_pid, status)
                        .await
                        .unwrap_or_else(|e| {
                            error!("failed to send pid exit event {}", e);
                        })
                } else {
                    break;
                }
            },
            _ => {}
        }
    }
}

async fn remove_socket_silently(address: &str) {
    remove_socket(address)
        .await
        .unwrap_or_else(|e| warn!("failed to remove socket: {}", e))
}

async fn remove_socket(address: &str) -> Result<()> {
    let path = parse_sockaddr(address);
    if let Ok(md) = Path::new(path).metadata() {
        if md.file_type().is_socket() {
            tokio::fs::remove_file(path).await.map_err(io_error!(
                e,
                "failed to remove socket {}",
                address
            ))?;
        }
    }
    Ok(())
}

async fn start_listener(address: &str) -> Result<UnixListener> {
    let addr = address.to_string();
    asyncify(move || -> Result<UnixListener> {
        crate::start_listener(&addr).map_err(|e| Error::IoError {
            context: format!("failed to start listener {}", addr),
            err: e,
        })
    })
    .await
}

async fn wait_socket_working(address: &str, interval_in_ms: u64, count: u32) -> Result<()> {
    for _i in 0..count {
        match Client::connect(address) {
            Ok(_) => {
                return Ok(());
            }
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_millis(interval_in_ms)).await;
            }
        }
    }
    Err(other!("time out waiting for socket {}", address))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::asynchronous::{start_listener, ExitSignal};

    #[tokio::test]
    async fn test_exit_signal() {
        let signal = Arc::new(ExitSignal::default());

        let cloned = signal.clone();
        let handle = tokio::spawn(async move {
            cloned.wait().await;
        });

        signal.signal();

        if let Err(err) = handle.await {
            panic!("{:?}", err);
        }
    }

    #[tokio::test]
    async fn test_start_listener() {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().to_str().unwrap().to_owned();

        let socket = path + "/ns1/id1/socket";
        let _listener = start_listener(&socket).await.unwrap();
        let _listener2 = start_listener(&socket)
            .await
            .expect_err("socket should already in use");

        let socket2 = socket + "/socket";
        assert!(start_listener(&socket2).await.is_err());

        let path = tmpdir.path().to_str().unwrap().to_owned();
        let txt_file = path + "/demo.txt";
        tokio::fs::write(&txt_file, "test").await.unwrap();
        assert!(start_listener(&txt_file).await.is_err());
        let context = tokio::fs::read_to_string(&txt_file).await.unwrap();
        assert_eq!(context, "test");
    }
}
