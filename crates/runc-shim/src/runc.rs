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

use std::{
    convert::TryFrom,
    os::{
        fd::{IntoRawFd, OwnedFd},
        unix::{
            io::{AsRawFd, FromRawFd},
            prelude::ExitStatusExt,
        },
    },
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use containerd_shim::{
    api::{CreateTaskRequest, ExecProcessRequest, Options, Status},
    asynchronous::monitor::{monitor_subscribe, monitor_unsubscribe, Subscription},
    io_error,
    monitor::{ExitEvent, Subject, Topic},
    mount::umount_recursive,
    other, other_error,
    protos::{
        api::ProcessInfo,
        cgroups::metrics::Metrics,
        protobuf::{CodedInputStream, Message},
    },
    util::{asyncify, mkdir, mount_rootfs, read_file_to_str, write_options, write_runtime},
    Console, Error, ExitSignal, Result,
};
use log::{debug, error};
use nix::{sys::signal::kill, unistd::Pid};
use oci_spec::runtime::{LinuxResources, Process};
use runc::{Command, Runc, Spawner};
use tokio::{
    fs::{remove_file, File, OpenOptions},
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, BufReader},
};

use super::{
    console::ConsoleSocket,
    container::{ContainerFactory, ContainerTemplate, ProcessFactory},
    processes::{ProcessLifecycle, ProcessTemplate},
};
use crate::{
    common::{
        check_kill_error, create_io, create_runc, get_spec_from_request, handle_file_open,
        receive_socket, CreateConfig, Log, ProcessIO, ShimExecutor, INIT_PID_FILE, LOG_JSON_FILE,
    },
    io::Stdio,
};

/// check the process is zombie
#[cfg(target_os = "linux")]
fn is_zombie_process(pid: i32) -> bool {
    if let Ok(status) = std::fs::read_to_string(format!("/proc/{}/status", pid)) {
        for line in status.lines() {
            if line.starts_with("State:") && line.contains('Z') {
                return true;
            }
        }
    }
    false
}

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
        let bundle = req.bundle();
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

        let rootfs_vec = req.rootfs().to_vec();
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

        let runc = create_runc(
            runtime,
            ns,
            bundle,
            &opts,
            Some(Arc::new(ShimExecutor::default())),
        )?;

        let id = req.id();
        let stdio = Stdio::new(req.stdin(), req.stdout(), req.stderr(), req.terminal());

        let mut init = InitProcess::new(
            id,
            stdio,
            RuncInitLifecycle::new(runc.clone(), opts.clone(), bundle),
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
            .pid_file(&pid_path)
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
            return Err(runtime_error(bundle, e, "OCI runtime create failed").await);
        }
        copy_io_or_console(init, socket, pio, init.lifecycle.exit_signal.clone()).await?;
        let pid = read_file_to_str(pid_path).await?.parse::<i32>()?;
        init.pid = pid;
        Ok(())
    }
}

// runtime_error will read the OCI runtime logfile retrieving OCI runtime error
pub async fn runtime_error(bundle: &str, e: runc::error::Error, msg: &str) -> Error {
    let mut rt_msg = String::new();
    match File::open(Path::new(bundle).join(LOG_JSON_FILE)).await {
        Err(err) => other!("{}: unable to open OCI runtime log file){}", msg, err),
        Ok(file) => {
            let mut lines = BufReader::new(file).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // Retrieve the last runtime error
                match serde_json::from_str::<Log>(&line) {
                    Err(err) => return other!("{}: unable to parse log msg: {}", msg, err),
                    Ok(log) => {
                        if log.level == "error" {
                            rt_msg = log.msg.trim().to_string();
                        }
                    }
                }
            }
            if !rt_msg.is_empty() {
                other!("{}: {}", msg, rt_msg)
            } else {
                other!("{}: (no OCI runtime error in logfile) {}", msg, e)
            }
        }
    }
}

pub struct RuncExecFactory {
    runtime: Runc,
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
            lifecycle: Arc::from(RuncExecLifecycle {
                runtime: self.runtime.clone(),
                bundle: self.bundle.to_string(),
                container_id: req.id.to_string(),
                io_uid: self.io_uid,
                io_gid: self.io_gid,
                spec: p,
                exit_signal: Default::default(),
            }),
            stdin: Arc::new(Mutex::new(None)),
        })
    }
}

pub struct RuncInitLifecycle {
    runtime: Runc,
    opts: Options,
    bundle: String,
    exit_signal: Arc<ExitSignal>,
}

#[async_trait]
impl ProcessLifecycle<InitProcess> for RuncInitLifecycle {
    async fn start(&self, p: &mut InitProcess) -> containerd_shim::Result<()> {
        if let Err(e) = self.runtime.start(p.id.as_str()).await {
            return Err(runtime_error(&p.lifecycle.bundle, e, "OCI runtime start failed").await);
        }
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
        if let Err(e) = self
            .runtime
            .delete(
                p.id.as_str(),
                Some(&runc::options::DeleteOpts { force: true }),
            )
            .await
        {
            if !e.to_string().to_lowercase().contains("does not exist") {
                return Err(
                    runtime_error(&p.lifecycle.bundle, e, "OCI runtime delete failed").await,
                );
            }
        }
        umount_recursive(Path::new(&self.bundle).join("rootfs").to_str(), 0)?;
        self.exit_signal.signal();
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn update(&self, p: &mut InitProcess, resources: &LinuxResources) -> Result<()> {
        if p.pid <= 0 {
            return Err(other!(
                "failed to update resources because init process is {}",
                p.pid
            ));
        }

        // check the process is zombie
        if is_zombie_process(p.pid) {
            return Err(other!(
                "failed to update resources because process {} is a zombie",
                p.pid
            ));
        }

        containerd_shim::cgroup::update_resources(p.pid as u32, resources)
    }

    #[cfg(not(target_os = "linux"))]
    async fn update(&self, _p: &mut InitProcess, _resources: &LinuxResources) -> Result<()> {
        Err(Error::Unimplemented("update resource".to_string()))
    }

    #[cfg(target_os = "linux")]
    async fn stats(&self, p: &InitProcess) -> Result<Metrics> {
        if p.pid <= 0 {
            return Err(other!(
                "failed to collect metrics because init process is {}",
                p.pid
            ));
        }

        // check the process is zombie
        if is_zombie_process(p.pid) {
            return Err(other!(
                "failed to collect metrics because process {} is a zombie",
                p.pid
            ));
        }

        containerd_shim::cgroup::collect_metrics(p.pid as u32)
    }

    #[cfg(not(target_os = "linux"))]
    async fn stats(&self, _p: &InitProcess) -> Result<Metrics> {
        Err(Error::Unimplemented("process stats".to_string()))
    }

    async fn ps(&self, p: &InitProcess) -> Result<Vec<ProcessInfo>> {
        let pids = self
            .runtime
            .ps(&p.id)
            .await
            .map_err(other_error!("failed to execute runc ps"))?;
        Ok(pids
            .iter()
            .map(|&x| ProcessInfo {
                pid: x as u32,
                ..Default::default()
            })
            .collect())
    }

    #[cfg(target_os = "linux")]
    async fn pause(&self, p: &mut InitProcess) -> Result<()> {
        match p.state {
            Status::RUNNING => {
                p.state = Status::PAUSING;
                if let Err(e) = self.runtime.pause(p.id.as_str()).await {
                    p.state = Status::RUNNING;
                    return Err(runtime_error(&self.bundle, e, "OCI runtime pause failed").await);
                }
                p.state = Status::PAUSED;
                Ok(())
            }
            _ => Err(other!("cannot pause when in {:?} state", p.state)),
        }
    }

    #[cfg(not(target_os = "linux"))]
    async fn pause(&self, _p: &mut InitProcess) -> Result<()> {
        Err(Error::Unimplemented("pause".to_string()))
    }

    #[cfg(target_os = "linux")]
    async fn resume(&self, p: &mut InitProcess) -> Result<()> {
        match p.state {
            Status::PAUSED => {
                if let Err(e) = self.runtime.resume(p.id.as_str()).await {
                    return Err(runtime_error(&self.bundle, e, "OCI runtime pause failed").await);
                }
                p.state = Status::RUNNING;
                Ok(())
            }
            _ => Err(other!("cannot resume when in {:?} state", p.state)),
        }
    }

    #[cfg(not(target_os = "linux"))]
    async fn resume(&self, _p: &mut InitProcess) -> Result<()> {
        Err(Error::Unimplemented("resume".to_string()))
    }
}

impl RuncInitLifecycle {
    pub fn new(runtime: Runc, opts: Options, bundle: &str) -> Self {
        let work_dir = Path::new(bundle).join("work");
        let mut opts = opts;
        if opts.criu_path().is_empty() {
            opts.criu_path = work_dir.to_string_lossy().to_string();
        }
        Self {
            runtime,
            opts,
            bundle: bundle.to_string(),
            exit_signal: Default::default(),
        }
    }
}

pub struct RuncExecLifecycle {
    runtime: Runc,
    bundle: String,
    container_id: String,
    io_uid: u32,
    io_gid: u32,
    spec: Process,
    exit_signal: Arc<ExitSignal>,
}

#[async_trait]
impl ProcessLifecycle<ExecProcess> for RuncExecLifecycle {
    async fn start(&self, p: &mut ExecProcess) -> containerd_shim::Result<()> {
        let bundle = self.bundle.to_string();
        let pid_path = Path::new(&bundle).join(format!("{}.pid", &p.id));
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
            return Err(runtime_error(&bundle, e, "OCI runtime exec failed").await);
        }

        if !p.stdio.stdin.is_empty() {
            let stdin_clone = p.stdio.stdin.clone();
            let stdin_w = p.stdin.clone();
            // Open the write side in advance to make sure read side will not block,
            // open it in another thread otherwise it will block too.
            tokio::spawn(async move {
                if let Ok(stdin_w_file) = OpenOptions::new()
                    .write(true)
                    .open(stdin_clone.as_str())
                    .await
                {
                    let mut lock_guard = stdin_w.lock().unwrap();
                    *lock_guard = Some(stdin_w_file);
                }
            });
        }

        copy_io_or_console(p, socket, pio, p.lifecycle.exit_signal.clone()).await?;
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
                Pid::from_raw(p.pid),
                nix::sys::signal::Signal::try_from(signal as i32).unwrap(),
            )
            .map_err(Into::into)
        }
    }

    async fn delete(&self, p: &mut ExecProcess) -> Result<()> {
        self.exit_signal.signal();
        let exec_pid_path = Path::new(self.bundle.as_str()).join(format!("{}.pid", p.id));
        remove_file(exec_pid_path).await.unwrap_or_default();
        Ok(())
    }

    async fn update(&self, _p: &mut ExecProcess, _resources: &LinuxResources) -> Result<()> {
        Err(Error::Unimplemented("exec update".to_string()))
    }

    async fn stats(&self, _p: &ExecProcess) -> Result<Metrics> {
        Err(Error::Unimplemented("exec stats".to_string()))
    }

    async fn ps(&self, _p: &ExecProcess) -> Result<Vec<ProcessInfo>> {
        Err(Error::Unimplemented("exec ps".to_string()))
    }

    async fn pause(&self, _p: &mut ExecProcess) -> Result<()> {
        Err(Error::Unimplemented("exec pause".to_string()))
    }

    async fn resume(&self, _p: &mut ExecProcess) -> Result<()> {
        Err(Error::Unimplemented("exec resume".to_string()))
    }
}

async fn copy_console(
    console_socket: &ConsoleSocket,
    stdio: &Stdio,
    exit_signal: Arc<ExitSignal>,
) -> Result<Console> {
    debug!("copy_console: waiting for runtime to send console fd");
    let stream = console_socket.accept().await?;
    let fd = asyncify(move || -> Result<OwnedFd> { receive_socket(stream.as_raw_fd()) }).await?;
    let f = unsafe { File::from_raw_fd(fd.into_raw_fd()) };
    if !stdio.stdin.is_empty() {
        debug!("copy_console: pipe stdin to console");
        let console_stdin = f
            .try_clone()
            .await
            .map_err(io_error!(e, "failed to clone console file"))?;
        let stdin = handle_file_open(|| async {
            OpenOptions::new()
                .read(true)
                .open(stdio.stdin.as_str())
                .await
        })
        .await
        .map_err(io_error!(e, "failed to open stdin"))?;
        spawn_copy(stdin, console_stdin, exit_signal.clone(), None::<fn()>);
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
            exit_signal,
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

pub async fn copy_io(pio: &ProcessIO, stdio: &Stdio, exit_signal: Arc<ExitSignal>) -> Result<()> {
    if !pio.copy {
        return Ok(());
    };
    if let Some(io) = &pio.io {
        if let Some(w) = io.stdin() {
            debug!("copy_io: pipe stdin from {}", stdio.stdin.as_str());
            if !stdio.stdin.is_empty() {
                let stdin = handle_file_open(|| async {
                    OpenOptions::new()
                        .read(true)
                        .open(stdio.stdin.as_str())
                        .await
                })
                .await
                .map_err(io_error!(e, "open stdin"))?;
                spawn_copy(stdin, w, exit_signal.clone(), None::<fn()>);
            }
        }

        if let Some(r) = io.stdout() {
            debug!("copy_io: pipe stdout from to {}", stdio.stdout.as_str());
            if !stdio.stdout.is_empty() {
                let stdout = handle_file_open(|| async {
                    OpenOptions::new()
                        .write(true)
                        .open(stdio.stdout.as_str())
                        .await
                })
                .await
                .map_err(io_error!(e, "open stdout"))?;
                // open a read to make sure even if the read end of containerd shutdown,
                // copy still continue until the restart of containerd succeed
                let stdout_r = handle_file_open(|| async {
                    OpenOptions::new()
                        .read(true)
                        .open(stdio.stdout.as_str())
                        .await
                })
                .await
                .map_err(io_error!(e, "open stdout for read"))?;
                spawn_copy(
                    r,
                    stdout,
                    exit_signal.clone(),
                    Some(move || {
                        drop(stdout_r);
                    }),
                );
            }
        }

        if let Some(r) = io.stderr() {
            if !stdio.stderr.is_empty() {
                debug!("copy_io: pipe stderr from to {}", stdio.stderr.as_str());
                let stderr = handle_file_open(|| async {
                    OpenOptions::new()
                        .write(true)
                        .open(stdio.stderr.as_str())
                        .await
                })
                .await
                .map_err(io_error!(e, "open stderr"))?;
                // open a read to make sure even if the read end of containerd shutdown,
                // copy still continue until the restart of containerd succeed
                let stderr_r = handle_file_open(|| async {
                    OpenOptions::new()
                        .read(true)
                        .open(stdio.stderr.as_str())
                        .await
                })
                .await
                .map_err(io_error!(e, "open stderr for read"))?;
                spawn_copy(
                    r,
                    stderr,
                    exit_signal,
                    Some(move || {
                        drop(stderr_r);
                    }),
                );
            }
        }
    }

    Ok(())
}

fn spawn_copy<R, W, F>(from: R, to: W, exit_signal: Arc<ExitSignal>, on_close: Option<F>)
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
    F: FnOnce() + Send + 'static,
{
    let mut src = from;
    let mut dst = to;
    tokio::spawn(async move {
        tokio::select! {
            _ = exit_signal.wait() => {
                debug!("container exit, copy task should exit too");
            },
            res = tokio::io::copy(&mut src, &mut dst) => {
               if let Err(e) = res {
                    error!("copy io failed {}", e);
                }
            }
        }
        if let Some(f) = on_close {
            f();
        }
    });
}

async fn copy_io_or_console<P>(
    p: &mut ProcessTemplate<P>,
    socket: Option<ConsoleSocket>,
    pio: Option<ProcessIO>,
    exit_signal: Arc<ExitSignal>,
) -> Result<()> {
    if p.stdio.terminal {
        if let Some(console_socket) = socket {
            let console_result = copy_console(&console_socket, &p.stdio, exit_signal).await;
            console_socket.clean().await;
            match console_result {
                Ok(c) => {
                    p.console = Some(c);
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }
    } else if let Some(pio) = pio {
        copy_io(&pio, &p.stdio, exit_signal).await?;
    }
    Ok(())
}

#[async_trait]
impl Spawner for ShimExecutor {
    async fn execute(&self, cmd: Command) -> runc::Result<(ExitStatus, u32, String, String)> {
        let mut cmd = cmd;
        let subscription = monitor_subscribe(Topic::Pid)
            .await
            .map_err(|e| runc::error::Error::Other(Box::new(e)))?;
        let sid = subscription.id;
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                monitor_unsubscribe(sid).await.unwrap_or_default();
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
        monitor_unsubscribe(sid).await.unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use std::{os::unix::process::ExitStatusExt, path::Path, process::ExitStatus};

    use containerd_shim::util::{mkdir, write_str_to_file};
    use runc::error::Error::CommandFailed;
    use tokio::fs::remove_dir_all;

    use crate::{common::LOG_JSON_FILE, runc::runtime_error};

    #[tokio::test]
    async fn test_runtime_error() {
        let empty_err = CommandFailed {
            status: ExitStatus::from_raw(1),
            stdout: "".to_string(),
            stderr: "".to_string(),
        };
        let log_json = "\
        {\"level\":\"info\",\"msg\":\"hello world\",\"time\":\"2022-11-25\"}\n\
        {\"level\":\"error\",\"msg\":\"failed error\",\"time\":\"2022-11-26\"}\n\
        {\"level\":\"error\",\"msg\":\"panic\",\"time\":\"2022-11-27\"}\n\
        ";
        let test_dir = "/tmp/shim-test";
        let _ = mkdir(test_dir, 0o744).await;
        write_str_to_file(Path::new(test_dir).join(LOG_JSON_FILE).as_path(), log_json)
            .await
            .expect("write log json should not be error");

        let expectd_msg = "panic";
        let actual_err = runtime_error(test_dir, empty_err, "").await;
        remove_dir_all(test_dir)
            .await
            .expect("remove test dir should not be error");
        assert!(
            actual_err.to_string().contains(expectd_msg),
            "actual error \"{}\" should contains \"{}\"",
            actual_err,
            expectd_msg
        );
    }
}
