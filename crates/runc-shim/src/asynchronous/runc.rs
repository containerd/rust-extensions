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

use std::convert::TryFrom;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::prelude::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use async_trait::async_trait;
use log::{debug, error};
use nix::sys::signal::kill;
use nix::unistd::Pid;
use oci_spec::runtime::{LinuxNamespaceType, Process};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};

use containerd_shim::api::{CreateTaskRequest, ExecProcessRequest, Options, Status};
use containerd_shim::asynchronous::console::ConsoleSocket;
use containerd_shim::asynchronous::container::{
    ContainerFactory, ContainerTemplate, ProcessFactory,
};
use containerd_shim::asynchronous::monitor::{
    monitor_subscribe, monitor_unsubscribe, Subscription,
};
use containerd_shim::asynchronous::processes::{ProcessLifecycle, ProcessTemplate};
use containerd_shim::asynchronous::utils::{
    asyncify, mkdir, mount_rootfs, read_file_to_str, read_spec, write_options, write_runtime,
};
use containerd_shim::console::Console;
use containerd_shim::io::Stdio;
use containerd_shim::monitor::{ExitEvent, Subject, Topic};
use containerd_shim::protos::protobuf::{CodedInputStream, Message};
use containerd_shim::{io_error, other, Error};
use containerd_shim::{other_error, Result};
use runc::{Command, Executor, Runc};

use crate::console::receive_socket;
use crate::io::{create_io, ProcessIO};
use crate::runc::{
    check_kill_error, create_runc, get_spec_from_request, CreateConfig, ShimExecutor, INIT_PID_FILE,
};

pub type ExecProcess = ProcessTemplate<RuncExecLifecycle>;
pub type InitProcess = ProcessTemplate<RuncInitLifecycle>;

pub type RuncContainer = ContainerTemplate<InitProcess, ExecProcess, RuncExecFactory>;

#[derive(Clone, Default)]
pub(crate) struct RuncFactory {}

#[async_trait]
impl ContainerFactory<RuncContainer> for RuncFactory {
    async fn create(
        &self,
        ns: &str,
        req: &CreateTaskRequest,
    ) -> containerd_shim::Result<RuncContainer> {
        let bundle = req.get_bundle();
        let mut opts = Options::new();
        if let Some(any) = req.options.as_ref() {
            let mut input = CodedInputStream::from_bytes(any.value.as_ref());
            opts.merge_from(&mut input)?;
        }
        if opts.compute_size() > 0 {
            debug!("create options: {:?}", &opts);
        }
        let runtime = opts.binary_name.as_str();
        write_options(bundle, &opts).await?;
        write_runtime(bundle, runtime).await?;

        let rootfs_vec = req.get_rootfs().to_vec();
        let rootfs = if !rootfs_vec.is_empty() {
            let tmp_rootfs = Path::new(bundle).join("rootfs");
            mkdir(&tmp_rootfs, 0o711).await?;
            tmp_rootfs
        } else {
            PathBuf::new()
        };

        for m in rootfs_vec {
            mount_rootfs(&m, rootfs.as_path()).await?
        }

        let runc = create_runc(runtime, ns, bundle, &opts, ShimExecutor {})?;

        let id = req.get_id();
        let stdio = Stdio::new(
            req.get_stdin(),
            req.get_stdout(),
            req.get_stderr(),
            req.get_terminal(),
        );

        let mut init = InitProcess::new(
            id,
            stdio,
            RuncInitLifecycle::new(runc.clone(), opts.clone(), rootfs, bundle),
        );

        let config = CreateConfig::default();
        self.do_create(&mut init, config).await?;
        let container = RuncContainer {
            id: id.to_string(),
            bundle: bundle.to_string(),
            init,
            process_factory: RuncExecFactory {
                runtime: runc,
                bundle: bundle.to_string(),
                io_uid: opts.io_uid,
                io_gid: opts.io_gid,
            },
            processes: Default::default(),
        };
        Ok(container)
    }

    async fn cleanup(&self, _ns: &str, _c: &RuncContainer) -> containerd_shim::Result<()> {
        Ok(())
    }
}

impl RuncFactory {
    async fn do_create(&self, init: &mut InitProcess, _config: CreateConfig) -> Result<()> {
        let id = init.id.to_string();
        let stdio = &init.stdio;
        let opts = &init.lifecycle.opts;
        let bundle = &init.lifecycle.bundle;
        let pid_path = Path::new(bundle).join(INIT_PID_FILE);
        let mut create_opts = runc::options::CreateOpts::new()
            .pid_file(pid_path.to_owned())
            .no_pivot(opts.no_pivot_root)
            .no_new_keyring(opts.no_new_keyring)
            .detach(false);
        let (socket, pio) = if stdio.terminal {
            let s = ConsoleSocket::new().await?;
            create_opts.console_socket = Some(s.path.to_owned());
            (Some(s), None)
        } else {
            let pio = create_io(&id, opts.io_uid, opts.io_gid, stdio)?;
            create_opts.io = pio.io.as_ref().cloned();
            (None, Some(pio))
        };

        let resp = init
            .lifecycle
            .runtime
            .create(&id, bundle, Some(&create_opts))
            .await;
        if let Err(e) = resp {
            if let Some(s) = socket {
                s.clean().await;
            }
            return Err(other!("failed to create runc container: {}", e));
        }
        copy_io_or_console(init, socket, pio).await?;
        let pid = read_file_to_str(pid_path).await?.parse::<i32>()?;
        init.pid = pid;
        Ok(())
    }
}

pub struct RuncExecFactory {
    runtime: Runc<ShimExecutor>,
    bundle: String,
    io_uid: u32,
    io_gid: u32,
}

#[async_trait]
impl ProcessFactory<ExecProcess> for RuncExecFactory {
    async fn create(&self, req: &ExecProcessRequest) -> Result<ExecProcess> {
        let p = get_spec_from_request(req)?;
        Ok(ExecProcess {
            state: Status::CREATED,
            id: req.exec_id.to_string(),
            stdio: Stdio {
                stdin: req.stdin.to_string(),
                stdout: req.stdout.to_string(),
                stderr: req.stderr.to_string(),
                terminal: req.terminal,
            },
            pid: 0,
            exit_code: 0,
            exited_at: None,
            wait_chan_tx: vec![],
            console: None,
            lifecycle: RuncExecLifecycle {
                runtime: self.runtime.clone(),
                bundle: self.bundle.to_string(),
                container_id: req.id.to_string(),
                io_uid: self.io_uid,
                io_gid: self.io_gid,
                spec: p,
            },
        })
    }
}

#[derive(Clone)]
pub struct RuncInitLifecycle {
    runtime: Runc<ShimExecutor>,
    opts: Options,
    bundle: String,
    rootfs: PathBuf,
    work_dir: PathBuf,
}

#[async_trait]
impl ProcessLifecycle<InitProcess> for RuncInitLifecycle {
    async fn start(&self, p: &mut InitProcess) -> containerd_shim::Result<()> {
        self.runtime
            .start(p.id.as_str())
            .await
            .map_err(other_error!(e, "failed start"))?;
        p.state = Status::RUNNING;
        Ok(())
    }

    async fn kill(
        &self,
        p: &mut InitProcess,
        signal: u32,
        all: bool,
    ) -> containerd_shim::Result<()> {
        self.runtime
            .kill(
                p.id.as_str(),
                signal,
                Some(&runc::options::KillOpts { all }),
            )
            .await
            .map_err(|e| check_kill_error(e.to_string()))
    }

    async fn delete(&self, p: &mut InitProcess) -> containerd_shim::Result<()> {
        self.runtime
            .delete(
                p.id.as_str(),
                Some(&runc::options::DeleteOpts { force: true }),
            )
            .await
            .or_else(|e| {
                if !e.to_string().to_lowercase().contains("does not exist") {
                    Err(e)
                } else {
                    Ok(())
                }
            })
            .map_err(other_error!(e, "failed delete"))
    }
}

impl RuncInitLifecycle {
    pub fn new(runtime: Runc<ShimExecutor>, opts: Options, rootfs: PathBuf, bundle: &str) -> Self {
        let work_dir = Path::new(bundle).join("work");
        let mut opts = opts;
        if opts.get_criu_path().is_empty() {
            opts.criu_path = work_dir.to_string_lossy().to_string();
        }
        Self {
            runtime,
            opts,
            bundle: bundle.to_string(),
            rootfs,
            work_dir,
        }
    }

    pub(crate) async fn should_kill_all_on_exit(&mut self, bundle_path: &str) -> bool {
        match read_spec(bundle_path).await {
            Ok(spec) => match spec.linux() {
                None => true,
                Some(linux) => match linux.namespaces() {
                    None => true,
                    Some(namespaces) => {
                        for ns in namespaces {
                            if ns.typ() == LinuxNamespaceType::Pid && ns.path().is_none() {
                                return false;
                            }
                        }
                        true
                    }
                },
            },
            Err(e) => {
                error!(
                    "failed to read spec when call should_kill_all_on_exit: {}",
                    e
                );
                false
            }
        }
    }
}

#[derive(Clone)]
pub struct RuncExecLifecycle {
    runtime: Runc<ShimExecutor>,
    bundle: String,
    container_id: String,
    io_uid: u32,
    io_gid: u32,
    spec: Process,
}

#[async_trait]
impl ProcessLifecycle<ExecProcess> for RuncExecLifecycle {
    async fn start(&self, p: &mut ExecProcess) -> containerd_shim::Result<()> {
        let pid_path = Path::new(self.bundle.as_str()).join(format!("{}.pid", &p.id));
        let mut exec_opts = runc::options::ExecOpts {
            io: None,
            pid_file: Some(pid_path.to_owned()),
            console_socket: None,
            detach: true,
        };
        let (socket, pio) = if p.stdio.terminal {
            let s = ConsoleSocket::new().await?;
            exec_opts.console_socket = Some(s.path.to_owned());
            (Some(s), None)
        } else {
            let pio = create_io(&p.id, self.io_uid, self.io_gid, &p.stdio)?;
            exec_opts.io = pio.io.as_ref().cloned();
            (None, Some(pio))
        };
        //TODO  checkpoint support
        let exec_result = self
            .runtime
            .exec(&self.container_id, &self.spec, Some(&exec_opts))
            .await;
        if let Err(e) = exec_result {
            if let Some(s) = socket {
                s.clean().await;
            }
            return Err(other!("failed to start runc exec: {}", e));
        }
        copy_io_or_console(p, socket, pio).await?;
        let pid = read_file_to_str(pid_path).await?.parse::<i32>()?;
        p.pid = pid;
        p.state = Status::RUNNING;
        Ok(())
    }

    async fn kill(
        &self,
        p: &mut ExecProcess,
        signal: u32,
        _all: bool,
    ) -> containerd_shim::Result<()> {
        if p.pid <= 0 {
            Err(Error::FailedPreconditionError(
                "process not created".to_string(),
            ))
        } else if p.exited_at.is_some() {
            Err(Error::NotFoundError("process already finished".to_string()))
        } else {
            // TODO this is kill from nix crate, it is os specific, maybe have annotated with target os
            kill(
                Pid::from_raw(p.pid as i32),
                nix::sys::signal::Signal::try_from(signal as i32).unwrap(),
            )
            .map_err(Into::into)
        }
    }

    async fn delete(&self, _p: &mut ExecProcess) -> containerd_shim::Result<()> {
        Ok(())
    }
}

async fn copy_console(console_socket: &ConsoleSocket, stdio: &Stdio) -> Result<Console> {
    debug!("copy_console: waiting for runtime to send console fd");
    let stream = console_socket.accept().await?;
    let fd = asyncify(move || -> Result<RawFd> { receive_socket(stream.as_raw_fd()) }).await?;
    let f = unsafe { File::from_raw_fd(fd) };
    if !stdio.stdin.is_empty() {
        debug!("copy_console: pipe stdin to console");
        let console_stdin = f
            .try_clone()
            .await
            .map_err(io_error!(e, "failed to clone console file"))?;
        let stdin = OpenOptions::new()
            .read(true)
            .write(true)
            .open(stdio.stdin.as_str())
            .await
            .map_err(io_error!(e, "open stdin"))?;
        spawn_copy(stdin, console_stdin, None::<fn()>);
    }

    if !stdio.stdout.is_empty() {
        let console_stdout = f
            .try_clone()
            .await
            .map_err(io_error!(e, "failed to clone console file"))?;
        debug!("copy_console: pipe stdout from console");
        let stdout = OpenOptions::new()
            .write(true)
            .open(stdio.stdout.as_str())
            .await
            .map_err(io_error!(e, "open stdout"))?;
        // open a read to make sure even if the read end of containerd shutdown,
        // copy still continue until the restart of containerd succeed
        let stdout_r = OpenOptions::new()
            .read(true)
            .open(stdio.stdout.as_str())
            .await
            .map_err(io_error!(e, "open stdout for read"))?;
        spawn_copy(
            console_stdout,
            stdout,
            Some(move || {
                drop(stdout_r);
            }),
        );
    }
    let console = Console {
        file: f.into_std().await,
    };
    Ok(console)
}

pub async fn copy_io(pio: &ProcessIO, stdio: &Stdio) -> Result<()> {
    if !pio.copy {
        return Ok(());
    };
    if let Some(io) = &pio.io {
        if let Some(w) = io.stdin() {
            debug!("copy_io: pipe stdin from {}", stdio.stdin.as_str());
            if !stdio.stdin.is_empty() {
                let stdin = OpenOptions::new()
                    .read(true)
                    .open(stdio.stdin.as_str())
                    .await
                    .map_err(io_error!(e, "open stdin"))?;
                //let closer = io.stdin().unwrap();
                spawn_copy(stdin, w, None::<fn()>);
            }
        }

        if let Some(r) = io.stdout() {
            debug!("copy_io: pipe stdout from to {}", stdio.stdout.as_str());
            if !stdio.stdout.is_empty() {
                let stdout = OpenOptions::new()
                    .write(true)
                    .open(stdio.stdout.as_str())
                    .await
                    .map_err(io_error!(e, "open stdout"))?;
                // open a read to make sure even if the read end of containerd shutdown,
                // copy still continue until the restart of containerd succeed
                let stdout_r = OpenOptions::new()
                    .read(true)
                    .open(stdio.stdout.as_str())
                    .await
                    .map_err(io_error!(e, "open stdout for read"))?;
                spawn_copy(
                    r,
                    stdout,
                    Some(move || {
                        drop(stdout_r);
                    }),
                );
            }
        }

        if let Some(r) = io.stderr() {
            if !stdio.stderr.is_empty() {
                debug!("copy_io: pipe stderr from to {}", stdio.stderr.as_str());
                let stderr = OpenOptions::new()
                    .write(true)
                    .open(stdio.stderr.as_str())
                    .await
                    .map_err(io_error!(e, "open stderr"))?;
                // open a read to make sure even if the read end of containerd shutdown,
                // copy still continue until the restart of containerd succeed
                let stderr_r = OpenOptions::new()
                    .read(true)
                    .open(stdio.stderr.as_str())
                    .await
                    .map_err(io_error!(e, "open stderr for read"))?;
                spawn_copy(
                    r,
                    stderr,
                    Some(move || {
                        drop(stderr_r);
                    }),
                );
            }
        }
    }

    Ok(())
}

fn spawn_copy<R, W, F>(from: R, to: W, on_close: Option<F>)
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
    F: FnOnce() + Send + 'static,
{
    let mut src = from;
    let mut dst = to;
    tokio::spawn(async move {
        tokio::io::copy(&mut src, &mut dst)
            .await
            .unwrap_or_else(|e| {
                error!("copy io failed {}", e);
                0
            });
        if let Some(f) = on_close {
            f();
        }
    });
}

async fn copy_io_or_console<P>(
    p: &mut ProcessTemplate<P>,
    socket: Option<ConsoleSocket>,
    pio: Option<ProcessIO>,
) -> Result<()> {
    if p.stdio.terminal {
        if let Some(console_socket) = socket {
            let console_result = copy_console(&console_socket, &p.stdio).await;
            match console_result {
                Ok(c) => {
                    p.console = Some(c);
                }
                Err(e) => {
                    console_socket.clean().await;
                    return Err(e);
                }
            }
        }
    } else if let Some(pio) = pio {
        copy_io(&pio, &p.stdio).await?;
    }
    Ok(())
}

#[async_trait]
impl Executor for ShimExecutor {
    async fn execute(&self, cmd: Command) -> runc::Result<(ExitStatus, u32, String, String)> {
        let mut cmd = cmd;
        let subscription = monitor_subscribe(Topic::Pid)
            .await
            .map_err(|e| runc::error::Error::Other(Box::new(e)))?;
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                monitor_unsubscribe(subscription.id)
                    .await
                    .unwrap_or_default();
                return Err(runc::error::Error::ProcessSpawnFailed(e));
            }
        };
        let pid = child.id().unwrap();
        let (stdout, stderr, exit_code) = tokio::join!(
            read_std(child.stdout),
            read_std(child.stderr),
            wait_pid(pid as i32, subscription)
        );
        let status = ExitStatus::from_raw(exit_code);
        Ok((status, pid, stdout, stderr))
    }
}

async fn read_std<T>(std: Option<T>) -> String
where
    T: AsyncRead + Unpin,
{
    let mut std = std;
    if let Some(mut std) = std.take() {
        let mut out = String::new();
        std.read_to_string(&mut out).await.unwrap_or_else(|e| {
            error!("failed to read stdout {}", e);
            0
        });
        return out;
    }
    "".to_string()
}

async fn wait_pid(pid: i32, s: Subscription) -> i32 {
    let mut s = s;
    loop {
        if let Some(ExitEvent {
            subject: Subject::Pid(epid),
            exit_code: code,
        }) = s.rx.recv().await
        {
            if pid == epid {
                monitor_unsubscribe(s.id).await.unwrap_or_default();
                return code;
            }
        }
    }
}
