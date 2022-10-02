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
#![allow(unused)]

use std::{
    convert::TryFrom,
    io::Read,
    os::unix::prelude::ExitStatusExt,
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{
        mpsc::{Receiver, SyncSender},
        Arc,
    },
};

use containerd_shim as shim;
use log::{debug, error};
use nix::{
    sys::{signal::kill, stat::Mode},
    unistd::{mkdir, Pid},
};
use oci_spec::runtime::{LinuxNamespaceType, LinuxResources};
use runc::{Command, Spawner};
use shim::{
    api::*,
    console::ConsoleSocket,
    error::{Error, Result},
    io::Stdio,
    monitor::{monitor_subscribe, wait_pid, ExitEvent, Subject, Subscription, Topic},
    mount::mount_rootfs,
    other, other_error,
    protos::{
        api::ProcessInfo,
        cgroups::metrics::Metrics,
        protobuf::{CodedInputStream, Message},
        shim::oci::ProcessDetails,
    },
    util::{convert_to_any, read_spec_from_file, write_options, write_runtime, IntoOption},
    Console,
};
use time::OffsetDateTime;

use crate::{
    common,
    common::{create_io, has_shared_pid_namespace, CreateConfig, ShimExecutor, INIT_PID_FILE},
    synchronous::container::{
        CommonContainer, CommonProcess, Container, ContainerFactory, Process,
    },
};

#[derive(Clone, Default)]
pub(crate) struct RuncFactory {}

impl ContainerFactory<RuncContainer> for RuncFactory {
    fn create(&self, ns: &str, req: &CreateTaskRequest) -> Result<RuncContainer> {
        let bundle = req.bundle.as_str();
        let mut opts = Options::new();
        if let Some(any) = req.options.as_ref() {
            let mut input = CodedInputStream::from_bytes(any.value.as_ref());
            opts.merge_from(&mut input)?;
        }
        if opts.compute_size() > 0 {
            debug!("create options: {:?}", &opts);
        }
        let runtime = opts.binary_name.as_str();
        write_options(bundle, &opts)?;
        write_runtime(bundle, runtime)?;

        let rootfs_vec = req.rootfs().to_vec();
        let rootfs = if !rootfs_vec.is_empty() {
            let tmp_rootfs = Path::new(bundle).join("rootfs");
            if !tmp_rootfs.as_path().exists() {
                mkdir(tmp_rootfs.as_path(), Mode::from_bits(0o711).unwrap())?;
            }
            tmp_rootfs
        } else {
            PathBuf::new()
        };
        let rootfs = rootfs
            .as_path()
            .to_str()
            .ok_or_else(|| other!("failed to convert rootfs to str"))?;
        for m in rootfs_vec {
            let mount_type = m.type_.as_str().none_if(|&x| x.is_empty());
            let source = m.source.as_str().none_if(|&x| x.is_empty());
            mount_rootfs(mount_type, source, &m.options.to_vec(), rootfs)?;
        }

        let runc = common::create_runc(
            runtime,
            ns,
            bundle,
            &opts,
            Some(Arc::new(ShimExecutor::default())),
        )?;

        let id = req.id();
        let stdio = Stdio {
            stdin: req.stdin().to_string(),
            stdout: req.stdout().to_string(),
            stderr: req.stderr().to_string(),
            terminal: req.terminal(),
        };

        let mut init = InitProcess::new(id, bundle, runc, stdio);
        init.rootfs = rootfs.to_string();
        let work_dir = Path::new(bundle).join("work");
        let work_dir = work_dir
            .as_path()
            .to_str()
            .ok_or_else(|| other!("failed to get work_dir str"))?;
        init.work_dir = work_dir.to_string();
        init.io_uid = opts.io_uid();
        init.io_gid = opts.io_gid();
        init.no_pivot_root = opts.no_pivot_root();
        init.no_new_key_ring = opts.no_new_keyring();
        init.criu_work_path = if opts.criu_path().is_empty() {
            work_dir.to_string()
        } else {
            opts.criu_path().to_string()
        };

        let config = CreateConfig::default();
        init.create(&config)?;
        let container = RuncContainer {
            common: CommonContainer {
                id: id.to_string(),
                bundle: bundle.to_string(),
                init,
                processes: Default::default(),
            },
        };
        Ok(container)
    }
}

pub(crate) struct RuncContainer {
    pub(crate) common: CommonContainer<InitProcess, ExecProcess>,
}

impl Container for RuncContainer {
    fn start(&mut self, exec_id: Option<&str>) -> Result<i32> {
        let id = self.id();
        match exec_id {
            Some(exec_id) => {
                let process = self
                    .common
                    .processes
                    .get_mut(exec_id)
                    .ok_or_else(|| other!("can not find the exec by id"))?;
                let pid_path = Path::new(self.common.bundle.as_str())
                    .join(format!("{}.pid", &process.common.id));

                let mut exec_opts = runc::options::ExecOpts {
                    io: None,
                    pid_file: Some(pid_path.to_owned()),
                    console_socket: None,
                    detach: true,
                };
                let terminal = process.common.stdio.terminal;
                let socket = if terminal {
                    let s = ConsoleSocket::new()?;
                    exec_opts.console_socket = Some(s.path.to_owned());
                    Some(s)
                } else {
                    let io = create_io(
                        &process.common.id,
                        self.common.init.io_uid,
                        self.common.init.io_gid,
                        &process.common.stdio,
                    )?;

                    process.common.io = Some(io);
                    exec_opts.io = process
                        .common
                        .io
                        .as_ref()
                        .map(|x| &x.io)
                        .unwrap_or(&None)
                        .clone();
                    None
                };
                //TODO  checkpoint support
                self.common
                    .init
                    .runtime
                    .exec(&id, &process.spec, Some(&exec_opts))
                    .map_err(other_error!(e, "failed exec"))?;
                if process.common.stdio.terminal {
                    let console_socket =
                        socket.ok_or_else(|| other!("failed to get console socket"))?;
                    let console = process.common.copy_console(&console_socket)?;
                    process.common.console = Some(console);
                } else {
                    process.common.copy_io()?;
                }
                process.common.set_pid_from_file(pid_path.as_path())?;
                process.common.state = Status::RUNNING;
                Ok(process.pid())
            }
            None => {
                self.common
                    .init
                    .runtime
                    .start(&id)
                    .map_err(other_error!(e, "failed start"))?;
                self.common.init.common.set_status(Status::RUNNING);
                Ok(self.pid())
            }
        }
    }

    fn state(&self, exec_id: Option<&str>) -> Result<StateResponse> {
        self.common.state(exec_id)
    }

    fn kill(&mut self, exec_id: Option<&str>, signal: u32, all: bool) -> Result<()> {
        match exec_id {
            Some(_) => {
                let p = self.common.get_mut_process(exec_id)?;
                kill_process(p.pid() as u32, p.exited_at(), signal)
                    .map_err(|e| common::check_kill_error(format!("{}", e)))
            }
            None => self
                .common
                .init
                .runtime
                .kill(
                    self.id().as_str(),
                    signal,
                    Some(&runc::options::KillOpts { all }),
                )
                .map_err(|e| common::check_kill_error(format!("{}", e))),
        }
    }

    fn wait_channel(&mut self, exec_id: Option<&str>) -> Result<Receiver<i8>> {
        self.common.wait_channel(exec_id)
    }

    fn get_exit_info(&self, exec_id: Option<&str>) -> Result<(i32, i32, Option<OffsetDateTime>)> {
        self.common.get_exit_info(exec_id)
    }

    fn delete(&mut self, exec_id_opt: Option<&str>) -> Result<(i32, i32, Option<OffsetDateTime>)> {
        let (pid, code, exited_at) = self
            .get_exit_info(exec_id_opt)
            .map_err(other_error!(e, "failed to get exit info"))?;
        match exec_id_opt {
            Some(exec_id) => {
                self.common.processes.remove(exec_id);
            }
            None => {
                self.common
                    .init
                    .runtime
                    .delete(
                        self.id().as_str(),
                        Some(&runc::options::DeleteOpts { force: true }),
                    )
                    .or_else(|e| {
                        if !e.to_string().to_lowercase().contains("does not exist") {
                            Err(e)
                        } else {
                            Ok(())
                        }
                    })
                    .map_err(other_error!(e, "failed delete"))?;
            }
        };
        Ok((pid, code, exited_at))
    }

    fn exec(&mut self, req: ExecProcessRequest) -> Result<()> {
        self.common
            .exec(req)
            .map_err(other_error!(e, "failed exec"))
    }

    fn resize_pty(&mut self, exec_id: Option<&str>, height: u32, width: u32) -> Result<()> {
        self.common
            .resize_pty(exec_id, height, width)
            .map_err(other_error!(e, "failed resize pty"))
    }

    fn pid(&self) -> i32 {
        self.common.init.pid()
    }

    #[cfg(target_os = "linux")]
    fn stats(&self) -> Result<Metrics> {
        let pid = self.common.init.pid() as u32;
        containerd_shim::cgroup::collect_metrics(pid)
    }

    #[cfg(not(target_os = "linux"))]
    fn stats(&self) -> Result<Metrics> {
        Err(Error::Unimplemented("stats".to_string()))
    }

    #[cfg(target_os = "linux")]
    fn update(&mut self, resources: &LinuxResources) -> Result<()> {
        let pid = self.common.init.pid() as u32;
        containerd_shim::cgroup::update_resources(pid, resources)
    }

    #[cfg(not(target_os = "linux"))]
    fn update(&mut self, resources: &LinuxResources) -> Result<()> {
        Err(Error::Unimplemented("update".to_string()))
    }

    fn pids(&self) -> Result<PidsResponse> {
        let pids = self
            .common
            .init
            .runtime
            .ps(self.common.init.id())
            .map_err(other_error!(e, "failed to ps"))?;
        let mut processes: Vec<ProcessInfo> = Vec::new();
        for pid in pids {
            let mut p_info = ProcessInfo {
                pid: pid as u32,
                ..Default::default()
            };
            for process in self.common.processes.values() {
                if process.common.pid as usize == pid {
                    let details = ProcessDetails {
                        exec_id: "".to_string(),
                        ..Default::default()
                    };
                    p_info.set_info(convert_to_any(Box::new(details))?);
                    break;
                }
            }
            processes.push(p_info);
        }
        let resp = PidsResponse {
            processes,
            ..Default::default()
        };
        Ok(resp)
    }

    fn id(&self) -> String {
        self.common.id.to_string()
    }
}

impl RuncContainer {
    pub(crate) fn should_kill_all_on_exit(&mut self, bundle_path: &str) -> bool {
        match read_spec_from_file(bundle_path) {
            Ok(spec) => has_shared_pid_namespace(&spec),
            Err(e) => {
                error!("should_kill_all_on_exit: {}", e);
                false
            }
        }
    }
}

fn kill_process(pid: u32, exit_at: Option<OffsetDateTime>, sig: u32) -> Result<()> {
    if pid == 0 {
        Err(Error::FailedPreconditionError(
            "process not created".to_string(),
        ))
    } else if exit_at.is_some() {
        Err(Error::NotFoundError("process already finished".to_string()))
    } else {
        kill(
            Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::try_from(sig as i32).unwrap(),
        )
        .map_err(Into::into)
    }
}

pub(crate) struct InitProcess {
    pub(crate) common: CommonProcess,
    pub(crate) bundle: String,
    pub(crate) runtime: runc::Runc,
    pub(crate) rootfs: String,
    pub(crate) work_dir: String,
    pub(crate) io_uid: u32,
    pub(crate) io_gid: u32,
    pub(crate) no_pivot_root: bool,
    pub(crate) no_new_key_ring: bool,
    pub(crate) criu_work_path: String,
}

impl InitProcess {
    pub fn new(id: &str, bundle: &str, runtime: runc::Runc, stdio: Stdio) -> Self {
        InitProcess {
            common: CommonProcess {
                state: Status::CREATED,
                id: id.to_string(),
                stdio,
                pid: 0,
                io: None,
                exit_code: 0,
                exited_at: None,
                wait_chan_tx: vec![],
                console: None,
            },
            bundle: bundle.to_string(),
            runtime,
            rootfs: "".to_string(),
            work_dir: "".to_string(),
            io_uid: 0,
            io_gid: 0,
            no_pivot_root: false,
            no_new_key_ring: false,
            criu_work_path: "".to_string(),
        }
    }

    pub fn create(&mut self, _conf: &CreateConfig) -> Result<()> {
        //TODO  checkpoint support
        let id = self.common.id.to_string();
        let terminal = self.common.stdio.terminal;
        let bundle = self.bundle.to_string();
        let pid_path = Path::new(&bundle).join(INIT_PID_FILE);
        let mut create_opts = runc::options::CreateOpts::new()
            .pid_file(&pid_path)
            .no_pivot(self.no_pivot_root)
            .no_new_keyring(self.no_new_key_ring)
            .detach(false);
        let socket = if terminal {
            let s = ConsoleSocket::new()?;
            create_opts.console_socket = Some(s.path.to_owned());
            Some(s)
        } else {
            let io = create_io(&id, self.io_uid, self.io_gid, &self.common.stdio)?;
            self.common.io = Some(io);
            create_opts.io = self
                .common
                .io
                .as_ref()
                .map(|x| &x.io)
                .unwrap_or(&None)
                .clone();
            None
        };

        self.runtime
            .create(&id, &bundle, Some(&create_opts))
            .map_err(other_error!(e, "failed create"))?;
        if terminal {
            let console_socket = socket.ok_or_else(|| other!("failed to get console socket"))?;
            let console = self.common.copy_console(&console_socket)?;
            self.common.console = Some(console);
        } else {
            self.common.copy_io()?;
        }
        self.common.set_pid_from_file(pid_path.as_path())?;
        Ok(())
    }
}

impl Process for InitProcess {
    fn set_exited(&mut self, exit_code: i32) {
        self.common.set_exited(exit_code);
    }

    fn id(&self) -> &str {
        self.common.id()
    }

    fn status(&self) -> Status {
        self.common.status()
    }

    fn set_status(&mut self, status: Status) {
        self.common.set_status(status)
    }

    fn pid(&self) -> i32 {
        self.common.pid()
    }

    fn terminal(&self) -> bool {
        self.common.terminal()
    }

    fn stdin(&self) -> String {
        self.common.stdin()
    }

    fn stdout(&self) -> String {
        self.common.stdout()
    }

    fn stderr(&self) -> String {
        self.common.stderr()
    }

    fn state(&self) -> StateResponse {
        self.common.state()
    }

    fn add_wait(&mut self, tx: SyncSender<i8>) {
        self.common.add_wait(tx)
    }

    fn exit_code(&self) -> i32 {
        self.common.exit_code()
    }

    fn exited_at(&self) -> Option<OffsetDateTime> {
        self.common.exited_at()
    }

    fn copy_console(&self, console_socket: &ConsoleSocket) -> Result<Console> {
        self.common.copy_console(console_socket)
    }

    fn copy_io(&self) -> Result<()> {
        self.common.copy_io()
    }

    fn set_pid_from_file(&mut self, pid_path: &Path) -> Result<()> {
        self.common.set_pid_from_file(pid_path)
    }

    fn resize_pty(&mut self, height: u32, width: u32) -> Result<()> {
        self.common.resize_pty(height, width)
    }
}

pub(crate) struct ExecProcess {
    pub(crate) common: CommonProcess,
    pub(crate) spec: oci_spec::runtime::Process,
}

impl Process for ExecProcess {
    fn set_exited(&mut self, exit_code: i32) {
        self.common.set_exited(exit_code);
    }

    fn id(&self) -> &str {
        self.common.id()
    }

    fn status(&self) -> Status {
        self.common.status()
    }

    fn set_status(&mut self, status: Status) {
        self.common.set_status(status)
    }

    fn pid(&self) -> i32 {
        self.common.pid()
    }

    fn terminal(&self) -> bool {
        self.common.terminal()
    }

    fn stdin(&self) -> String {
        self.common.stdin()
    }

    fn stdout(&self) -> String {
        self.common.stdout()
    }

    fn stderr(&self) -> String {
        self.common.stderr()
    }

    fn state(&self) -> StateResponse {
        self.common.state()
    }

    fn add_wait(&mut self, tx: SyncSender<i8>) {
        self.common.add_wait(tx)
    }

    fn exit_code(&self) -> i32 {
        self.common.exit_code()
    }

    fn exited_at(&self) -> Option<OffsetDateTime> {
        self.common.exited_at()
    }

    fn copy_console(&self, console_socket: &ConsoleSocket) -> Result<Console> {
        self.common.copy_console(console_socket)
    }

    fn copy_io(&self) -> Result<()> {
        self.common.copy_io()
    }

    fn set_pid_from_file(&mut self, pid_path: &Path) -> Result<()> {
        self.common.set_pid_from_file(pid_path)
    }

    fn resize_pty(&mut self, height: u32, width: u32) -> Result<()> {
        self.common.resize_pty(height, width)
    }
}

impl TryFrom<ExecProcessRequest> for ExecProcess {
    type Error = Error;
    fn try_from(req: ExecProcessRequest) -> std::result::Result<Self, Self::Error> {
        let p = common::get_spec_from_request(&req)?;
        let exec_process = ExecProcess {
            common: CommonProcess {
                state: Status::CREATED,
                id: req.exec_id,
                stdio: Stdio {
                    stdin: req.stdin,
                    stdout: req.stdout,
                    stderr: req.stderr,
                    terminal: req.terminal,
                },
                pid: 0,
                io: None,
                exit_code: 0,
                exited_at: None,
                wait_chan_tx: vec![],
                console: None,
            },
            spec: p,
        };
        Ok(exec_process)
    }
}

impl Spawner for ShimExecutor {
    fn execute(&self, cmd: Command) -> runc::Result<(ExitStatus, u32, String, String)> {
        let mut cmd = cmd;
        let subscription =
            monitor_subscribe(Topic::Pid).map_err(|e| runc::error::Error::Other(Box::new(e)))?;
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Err(runc::error::Error::ProcessSpawnFailed(e));
            }
        };
        let pid = child.id();
        // May block here when stream exceeds buffer size, it's better to spawn another thread for io copy
        let (stdout, stderr, exit_code) = (
            read_std(child.stdout),
            read_std(child.stderr),
            wait_pid(pid as i32, subscription),
        );
        let status = ExitStatus::from_raw(exit_code);
        Ok((status, pid, stdout, stderr))
    }
}

fn read_std<T>(std: Option<T>) -> String
where
    T: Read,
{
    let mut std = std;
    if let Some(mut std) = std.take() {
        let mut out = String::new();
        std.read_to_string(&mut out).unwrap_or_else(|e| {
            error!("failed to read stdout {}", e);
            0
        });
        return out;
    }
    "".to_string()
}
