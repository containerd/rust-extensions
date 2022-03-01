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

use std::collections::HashMap;
use std::convert::TryFrom;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender};

use containerd_shim as shim;

#[cfg(target_os = "linux")]
use cgroups_rs::{cgroup, hierarchies, Cgroup, MaxValue, Subsystem};
use nix::sys::signal::kill;
use nix::sys::stat::Mode;
use nix::unistd::{mkdir, Pid};
use oci_spec::runtime::{Linux, LinuxNamespaceType, LinuxResources, Spec};
use runc::console::{Console, ConsoleSocket};
use runc::options::{CreateOpts, DeleteOpts, ExecOpts, GlobalOpts, KillOpts};
use runc::utils::new_temp_console_socket;
use shim::api::*;
use shim::error::{Error, Result};
use shim::io_error;
use shim::mount::mount_rootfs;
use shim::protos::api::ProcessInfo;
use shim::protos::cgroups::metrics::{CPUStat, CPUUsage, MemoryEntry, MemoryStat, Metrics};
use shim::protos::protobuf::well_known_types::{Any, Timestamp};
use shim::protos::protobuf::{CodedInputStream, Message, RepeatedField};
use shim::protos::shim::oci::ProcessDetails;
use shim::util::{read_spec_from_file, write_options, write_runtime, IntoOption};
use shim::{debug, error, other, other_error};
use time::OffsetDateTime;

use crate::container::{CommonContainer, CommonProcess, Container, ContainerFactory, Process};
use crate::io::{create_io, Stdio};

pub const DEFAULT_RUNC_ROOT: &str = "/run/containerd/runc";
const INIT_PID_FILE: &str = "init.pid";
const DEFAULT_COMMAND: &str = "runc";

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
        let mut runtime = opts.binary_name.as_str();
        write_options(bundle, &opts)?;
        write_runtime(bundle, runtime)?;

        let rootfs_vec = req.get_rootfs().to_vec();
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
            let mount_type = m.field_type.as_str().none_if(|&x| x.is_empty());
            let source = m.source.as_str().none_if(|&x| x.is_empty());
            mount_rootfs(mount_type, source, &m.options.to_vec(), rootfs)?;
        }

        let runc = {
            if runtime.is_empty() {
                runtime = DEFAULT_COMMAND;
            }
            let root = opts.root.as_str();
            let root = Path::new(if root.is_empty() {
                DEFAULT_RUNC_ROOT
            } else {
                root
            })
            .join(ns);
            let log_buf = Path::new(bundle).join("log.json");
            GlobalOpts::default()
                .command(runtime)
                .root(root)
                .log(log_buf)
                .systemd_cgroup(opts.get_systemd_cgroup())
                .log_json()
                .build()
                .map_err(other_error!(e, "unable to create runc instance"))?
        };

        let id = req.get_id();
        let stdio = Stdio {
            stdin: req.get_stdin().to_string(),
            stdout: req.get_stdout().to_string(),
            stderr: req.get_stderr().to_string(),
            terminal: req.get_terminal(),
        };

        let mut init = InitProcess::new(id, bundle, runc, stdio);
        init.rootfs = rootfs.to_string();
        let work_dir = Path::new(bundle).join("work");
        let work_dir = work_dir
            .as_path()
            .to_str()
            .ok_or_else(|| other!("failed to get work_dir str"))?;
        init.work_dir = work_dir.to_string();
        init.io_uid = opts.get_io_uid();
        init.io_gid = opts.get_io_gid();
        init.no_pivot_root = opts.get_no_pivot_root();
        init.no_new_key_ring = opts.get_no_new_keyring();
        init.criu_work_path = if opts.get_criu_path().is_empty() {
            work_dir.to_string()
        } else {
            opts.get_criu_path().to_string()
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
    #[cfg(not(feature = "async"))]
    fn start(&mut self, exec_id: Option<&str>) -> Result<i32> {
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
                    let s = new_temp_console_socket().map_err(other_error!(e, ""))?;
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
                    .exec(&self.common.id, &process.spec, Some(&exec_opts))
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
                Ok(process.common.pid())
            }
            None => {
                self.common
                    .init
                    .runtime
                    .start(self.common.id.as_str())
                    .map_err(other_error!(e, "failed start"))?;
                self.common.init.common.set_status(Status::RUNNING);
                Ok(self.common.init.common.pid())
            }
        }
    }

    #[cfg(feature = "async")]
    fn start(&mut self, exec_id: Option<&str>) -> Result<i32> {
        Err(Error::Unimplemented("start".to_string()))
    }

    fn state(&self, exec_id: Option<&str>) -> Result<StateResponse> {
        self.common.state(exec_id)
    }

    #[cfg(not(feature = "async"))]
    fn kill(&mut self, exec_id: Option<&str>, signal: u32, all: bool) -> Result<()> {
        match exec_id {
            Some(_) => {
                let p = self.common.get_mut_process(exec_id)?;
                kill_process(p.pid() as u32, p.exited_at(), signal)
                    .map_err(|e| check_kill_error(format!("{}", e)))
            }
            None => self
                .common
                .init
                .runtime
                .kill(
                    self.common.id.as_str(),
                    signal,
                    Some(&runc::options::KillOpts { all }),
                )
                .map_err(|e| check_kill_error(format!("{}", e))),
        }
    }

    #[cfg(feature = "async")]
    fn kill(&mut self, exec_id: Option<&str>, signal: u32, all: bool) -> Result<()> {
        Err(Error::Unimplemented("kill".to_string()))
    }

    fn wait_channel(&mut self, exec_id: Option<&str>) -> Result<Receiver<i8>> {
        self.common.wait_channel(exec_id)
    }

    fn get_exit_info(&self, exec_id: Option<&str>) -> Result<(i32, i32, Option<OffsetDateTime>)> {
        self.common.get_exit_info(exec_id)
    }

    #[cfg(not(feature = "async"))]
    fn delete(&mut self, exec_id_opt: Option<&str>) -> Result<(i32, u32, Timestamp)> {
        let (pid, code, exit_at) = self
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
                        self.common.id.as_str(),
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

        let mut time_stamp = Timestamp::new();
        if let Some(exit_at) = exit_at {
            time_stamp.set_seconds(exit_at.unix_timestamp());
            time_stamp.set_nanos(exit_at.nanosecond() as i32);
        }
        Ok((pid, code as u32, time_stamp))
    }

    #[cfg(feature = "async")]
    fn delete(&mut self, exec_id_opt: Option<&str>) -> Result<(i32, u32, Timestamp)> {
        Err(Error::Unimplemented("delete".to_string()))
    }

    #[cfg(not(feature = "async"))]
    fn exec(&mut self, req: ExecProcessRequest) -> Result<()> {
        self.common
            .exec(req)
            .map_err(other_error!(e, "failed exec"))
    }

    #[cfg(feature = "async")]
    fn exec(&mut self, req: ExecProcessRequest) -> Result<()> {
        Err(Error::Unimplemented("exec".to_string()))
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
        let mut metrics = Metrics::new();
        // get container main process cgroup
        let path = get_cgroups_relative_paths_by_pid(self.common.init.pid() as u32)?;
        let cgroup = Cgroup::load_with_relative_paths(hierarchies::auto(), Path::new("."), path);

        // to make it easy, fill the necessary metrics only.
        for sub_system in Cgroup::subsystems(&cgroup) {
            match sub_system {
                Subsystem::CpuAcct(cpuacct_ctr) => {
                    let mut cpu_usage = CPUUsage::new();
                    cpu_usage.set_total(cpuacct_ctr.cpuacct().usage);
                    let mut cpu_stat = CPUStat::new();
                    cpu_stat.set_usage(cpu_usage);
                    metrics.set_cpu(cpu_stat);
                }
                Subsystem::Mem(mem_ctr) => {
                    let mem = mem_ctr.memory_stat();
                    let mut mem_entry = MemoryEntry::new();
                    mem_entry.set_usage(mem.usage_in_bytes);
                    let mut mem_stat = MemoryStat::new();
                    mem_stat.set_usage(mem_entry);
                    mem_stat.set_total_inactive_file(mem.stat.total_inactive_file);
                    metrics.set_memory(mem_stat);
                }
                _ => {}
            }
        }
        Ok(metrics)
    }

    #[cfg(not(target_os = "linux"))]
    fn stats(&self) -> Result<Metrics> {
        Err(Error::Unimplemented("stats".to_string()))
    }

    #[cfg(target_os = "linux")]
    fn update(&mut self, resources: &LinuxResources) -> Result<()> {
        // get container main process cgroup
        let path = get_cgroups_relative_paths_by_pid(self.common.init.pid() as u32)?;
        let cgroup = Cgroup::load_with_relative_paths(hierarchies::auto(), Path::new("."), path);

        for sub_system in Cgroup::subsystems(&cgroup) {
            match sub_system {
                Subsystem::Pid(pid_ctr) => {
                    // set maximum number of PIDs
                    if let Some(pids) = resources.pids() {
                        pid_ctr
                            .set_pid_max(MaxValue::Value(pids.limit()))
                            .map_err(other_error!(e, "set pid max"))?;
                    }
                }
                Subsystem::Mem(mem_ctr) => {
                    if let Some(memory) = resources.memory() {
                        // set memory limit in bytes
                        if let Some(limit) = memory.limit() {
                            mem_ctr
                                .set_limit(limit)
                                .map_err(other_error!(e, "set mem limit"))?;
                        }

                        // set memory swap limit in bytes
                        if let Some(swap) = memory.swap() {
                            mem_ctr
                                .set_memswap_limit(swap)
                                .map_err(other_error!(e, "set memsw limit"))?;
                        }
                    }
                }
                Subsystem::CpuSet(cpuset_ctr) => {
                    if let Some(cpu) = resources.cpu() {
                        // set CPUs to use within the cpuset
                        if let Some(cpus) = cpu.cpus() {
                            cpuset_ctr
                                .set_cpus(cpus)
                                .map_err(other_error!(e, "set CPU sets"))?;
                        }

                        // set list of memory nodes in the cpuset
                        if let Some(mems) = cpu.mems() {
                            cpuset_ctr
                                .set_mems(mems)
                                .map_err(other_error!(e, "set CPU memes"))?;
                        }
                    }
                }
                Subsystem::Cpu(cpu_ctr) => {
                    if let Some(cpu) = resources.cpu() {
                        // set CPU shares
                        if let Some(shares) = cpu.shares() {
                            cpu_ctr
                                .set_shares(shares)
                                .map_err(other_error!(e, "set CPU share"))?;
                        }

                        // set CPU hardcap limit
                        if let Some(quota) = cpu.quota() {
                            cpu_ctr
                                .set_cfs_quota(quota)
                                .map_err(other_error!(e, "set CPU quota"))?;
                        }

                        // set CPU hardcap period
                        if let Some(period) = cpu.period() {
                            cpu_ctr
                                .set_cfs_period(period)
                                .map_err(other_error!(e, "set CPU period"))?;
                        }
                    }
                }
                Subsystem::HugeTlb(ht_ctr) => {
                    // set the limit if "pagesize" hugetlb usage
                    if let Some(hp_limits) = resources.hugepage_limits() {
                        for limit in hp_limits {
                            ht_ctr
                                .set_limit_in_bytes(
                                    limit.page_size().as_str(),
                                    limit.limit() as u64,
                                )
                                .map_err(other_error!(e, "set huge page limit"))?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn update(&mut self, resources: &LinuxResources) -> Result<()> {
        Err(Error::Unimplemented("update".to_string()))
    }

    #[cfg(feature = "async")]
    fn pids(&self) -> Result<PidsResponse> {
        Err(Error::Unimplemented("pids".to_string()))
    }

    #[cfg(not(feature = "async"))]
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
                    // marshal ProcessDetails to Any
                    let mut any = Any::new();
                    let mut data = Vec::new();
                    details
                        .write_to_vec(&mut data)
                        .map_err(other_error!(e, "write ProcessDetails to vec"))?;
                    any.set_value(data);
                    any.set_type_url(details.descriptor().full_name().to_string());

                    p_info.set_info(any);
                    break;
                }
            }
            processes.push(p_info);
        }
        let resp = PidsResponse {
            processes: RepeatedField::from(processes),
            ..Default::default()
        };
        Ok(resp)
    }
}

impl RuncContainer {
    pub(crate) fn should_kill_all_on_exit(&mut self, bundle_path: &str) -> bool {
        match read_spec_from_file(bundle_path) {
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
    #[cfg(not(feature = "async"))]
    pub fn create(&mut self, _conf: &CreateConfig) -> Result<()> {
        //TODO  checkpoint support
        let id = self.common.id.to_string();
        let terminal = self.common.stdio.terminal;
        let bundle = self.bundle.to_string();
        let pid_path = Path::new(&bundle).join(INIT_PID_FILE);
        let mut create_opts = runc::options::CreateOpts::new()
            .pid_file(pid_path.to_owned())
            .no_pivot(self.no_pivot_root)
            .no_new_keyring(self.no_new_key_ring)
            .detach(false);
        let socket = if terminal {
            let s = new_temp_console_socket().map_err(other_error!(e, ""))?;
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

    #[cfg(feature = "async")]
    pub fn create(&mut self, _conf: &CreateConfig) -> Result<()> {
        Err(Error::Unimplemented("create".to_string()))
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
        let p = get_spec_from_request(&req)?;
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

pub fn get_spec_from_request(req: &ExecProcessRequest) -> Result<oci_spec::runtime::Process> {
    if let Some(val) = req.spec.as_ref() {
        let mut p = serde_json::from_slice::<oci_spec::runtime::Process>(val.get_value())?;
        p.set_terminal(Some(req.terminal));
        Ok(p)
    } else {
        Err(Error::InvalidArgument("no spec in request".to_string()))
    }
}

#[derive(Default)]
pub(crate) struct CreateConfig {}

fn check_kill_error(emsg: String) -> Error {
    let emsg = emsg.to_lowercase();
    if emsg.contains("process already finished")
        || emsg.contains("container not running")
        || emsg.contains("no such process")
    {
        Error::NotFoundError("process already finished".to_string())
    } else if emsg.contains("does not exist") {
        Error::NotFoundError("no such container".to_string())
    } else {
        other!(emsg, "unknown error after kill")
    }
}

#[cfg(target_os = "linux")]
fn get_cgroups_relative_paths_by_pid(pid: u32) -> Result<HashMap<String, String>> {
    let path = format!("/proc/{}/cgroup", pid);
    let mut m = HashMap::new();
    let content = std::fs::read_to_string(path).map_err(io_error!(e, "read process cgroup"))?;
    for l in content.lines() {
        let fl: Vec<&str> = l.split(':').collect();
        if fl.len() != 3 {
            continue;
        }

        let keys: Vec<&str> = fl[1].split(',').collect();
        for key in &keys {
            m.insert(key.to_string(), fl[2].to_string());
        }
    }
    Ok(m)
}
