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
use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use containerd_shim::{
    api::{
        CreateTaskRequest, CreateTaskResponse, DeleteRequest, Empty, ExecProcessRequest,
        KillRequest, ResizePtyRequest, ShutdownRequest, StartRequest, StartResponse, StateRequest,
        StateResponse, Status, WaitRequest, WaitResponse,
    },
    asynchronous::ExitSignal,
    event::Event,
    protos::{
        api::{
            CloseIORequest, ConnectRequest, ConnectResponse, DeleteResponse, PidsRequest,
            PidsResponse, StatsRequest, StatsResponse, UpdateTaskRequest,
        },
        events::task::{TaskCreate, TaskDelete, TaskExecAdded, TaskExecStarted, TaskIO, TaskStart},
        protobuf::{EnumOrUnknown, MessageDyn},
        shim_async::Task,
        ttrpc::{self, r#async::TtrpcContext},
    },
    util::{convert_to_any, convert_to_timestamp, AsOption},
    TtrpcResult,
};
use log::{debug, info, warn};
use oci_spec::runtime::LinuxResources;
use tokio::sync::{
    mpsc::Sender, RwLock, RwLockMappedWriteGuard, RwLockReadGuard, RwLockWriteGuard,
};

use super::container::{Container, ContainerFactory};
type EventSender = Sender<(String, Box<dyn MessageDyn>)>;

#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "linux")]
use cgroups_rs::fs::hierarchies::is_cgroup2_unified_mode;
use containerd_shim::{
    api::{PauseRequest, ResumeRequest},
    protos::events::task::{TaskPaused, TaskResumed},
};
#[cfg(target_os = "linux")]
use containerd_shim::{
    error::{Error, Result},
    other_error,
    protos::events::task::TaskOOM,
};
#[cfg(target_os = "linux")]
use log::error;
#[cfg(target_os = "linux")]
use tokio::{sync::mpsc::Receiver, task::spawn};

#[cfg(target_os = "linux")]
use crate::cgroup_memory;

/// TaskService is a Task template struct, it is considered a helper struct,
/// which has already implemented `Task` trait, so that users can make it the type `T`
/// parameter of `Service`, and implements their own `ContainerFactory` and `Container`.
pub struct TaskService<F, C> {
    pub factory: F,
    // In comparison, a Mutex does not distinguish between readers or writers that acquire the lock,
    // therefore causing any tasks waiting for the lock to become available to yield.
    // An RwLock will allow any number of readers to acquire the lock as long as a writer is not holding the lock.
    pub containers: Arc<RwLock<HashMap<String, C>>>,
    pub namespace: String,
    pub exit: Arc<ExitSignal>,
    pub tx: EventSender,
}

impl<F, C> TaskService<F, C>
where
    F: Default,
{
    pub fn new(ns: &str, exit: Arc<ExitSignal>, tx: EventSender) -> Self {
        Self {
            factory: Default::default(),
            containers: Arc::new(RwLock::new(Default::default())),
            namespace: ns.to_string(),
            exit,
            tx,
        }
    }
}

impl<F, C> TaskService<F, C> {
    pub async fn container_mut(&self, id: &str) -> TtrpcResult<RwLockMappedWriteGuard<'_, C>> {
        let mut containers = self.containers.write().await;
        containers.get_mut(id).ok_or_else(|| {
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::NOT_FOUND,
                format!("can not find container by id {}", id),
            ))
        })?;
        let container = RwLockWriteGuard::map(containers, |m| m.get_mut(id).unwrap());
        Ok(container)
    }

    pub async fn container(&self, id: &str) -> TtrpcResult<RwLockReadGuard<'_, C>> {
        let containers = self.containers.read().await;
        containers.get(id).ok_or_else(|| {
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::NOT_FOUND,
                format!("can not find container by id {}", id),
            ))
        })?;
        let container = RwLockReadGuard::map(containers, |m| m.get(id).unwrap());
        Ok(container)
    }

    pub async fn send_event(&self, event: impl Event) {
        let topic = event.topic();
        self.tx
            .send((topic.to_string(), Box::new(event)))
            .await
            .unwrap_or_else(|e| warn!("send {} to publisher: {}", topic, e));
    }
}

#[cfg(target_os = "linux")]
fn run_oom_monitor(mut rx: Receiver<String>, id: String, tx: EventSender) {
    let oom_event = TaskOOM {
        container_id: id,
        ..Default::default()
    };
    let topic = oom_event.topic();
    let oom_box = Box::new(oom_event);
    spawn(async move {
        while let Some(_item) = rx.recv().await {
            tx.send((topic.to_string(), oom_box.clone()))
                .await
                .unwrap_or_else(|e| warn!("send {} to publisher: {}", topic, e));
        }
    });
}

#[cfg(target_os = "linux")]
async fn monitor_oom(id: &String, pid: u32, tx: EventSender) -> Result<()> {
    if !is_cgroup2_unified_mode() {
        let path_from_cgorup = cgroup_memory::get_path_from_cgorup(pid).await?;
        let (mount_root, mount_point) =
            cgroup_memory::get_existing_cgroup_mem_path(path_from_cgorup).await?;

        let mem_cgroup_path = mount_point + &mount_root;
        let rx = cgroup_memory::register_memory_event(
            id,
            Path::new(&mem_cgroup_path),
            "memory.oom_control",
        )
        .await
        .map_err(other_error!("register_memory_event failed:"))?;

        run_oom_monitor(rx, id.to_string(), tx);
    }
    Ok(())
}

#[async_trait]
impl<F, C> Task for TaskService<F, C>
where
    F: ContainerFactory<C> + Sync + Send,
    C: Container + Sync + Send + 'static,
{
    async fn state(&self, _ctx: &TtrpcContext, req: StateRequest) -> TtrpcResult<StateResponse> {
        let container = self.container(req.id()).await?;
        let exec_id = req.exec_id().as_option();
        let resp = container.state(exec_id).await?;
        Ok(resp)
    }

    async fn create(
        &self,
        _ctx: &TtrpcContext,
        req: CreateTaskRequest,
    ) -> TtrpcResult<CreateTaskResponse> {
        info!("Create request for {:?}", &req);
        // Note: Get containers here is for getting the lock,
        // to make sure no other threads manipulate the containers metadata;
        let ns = self.namespace.as_str();
        let id = req.id.as_str();
        let mut resp = CreateTaskResponse::new();
        let pid = {
            let mut containers = self.containers.write().await;
            let container = self.factory.create(ns, &req).await?;
            let pid = container.pid().await as u32;
            resp.pid = pid;
            containers.insert(id.to_string(), container);
            pid
        };

        self.send_event(TaskCreate {
            container_id: req.id.to_string(),
            bundle: req.bundle.to_string(),
            rootfs: req.rootfs,
            io: Some(TaskIO {
                stdin: req.stdin.to_string(),
                stdout: req.stdout.to_string(),
                stderr: req.stderr.to_string(),
                terminal: req.terminal,
                ..Default::default()
            })
            .into(),
            checkpoint: req.checkpoint.to_string(),
            pid,
            ..Default::default()
        })
        .await;
        info!("Create request for {} returns pid {}", id, resp.pid);
        Ok(resp)
    }

    async fn start(&self, _ctx: &TtrpcContext, req: StartRequest) -> TtrpcResult<StartResponse> {
        info!("Start request for {:?}", &req);
        let pid = {
            let mut container = self.container_mut(req.id()).await?;
            // Prevent the init process from exiting and continuing with start
            // Return early to reduce the time it takes to return only when runc encounters an error
            if container.init_state().await == EnumOrUnknown::new(Status::STOPPED) {
                debug!("container init process has exited, start process should not continue");
                return Err(ttrpc::Error::RpcStatus(ttrpc::get_status(
                    ttrpc::Code::FAILED_PRECONDITION,
                    format!("container init process has exited {}", container.id().await),
                )));
            }
            container.start(req.exec_id.as_str().as_option()).await?
        };

        let mut resp = StartResponse::new();
        resp.pid = pid as u32;

        if req.exec_id.is_empty() {
            self.send_event(TaskStart {
                container_id: req.id.to_string(),
                pid: pid as u32,
                ..Default::default()
            })
            .await;
            #[cfg(target_os = "linux")]
            if let Err(e) = monitor_oom(&req.id, resp.pid, self.tx.clone()).await {
                error!("monitor_oom failed: {:?}.", e);
            }
        } else {
            self.send_event(TaskExecStarted {
                container_id: req.id.to_string(),
                exec_id: req.exec_id.to_string(),
                pid: pid as u32,
                ..Default::default()
            })
            .await;
        };

        info!("Start request for {:?} returns pid {}", req, resp.pid());
        Ok(resp)
    }

    async fn delete(&self, _ctx: &TtrpcContext, req: DeleteRequest) -> TtrpcResult<DeleteResponse> {
        info!("Delete request for {:?}", &req);
        let (id, pid, exit_status, exited_at) = {
            let mut container = self.container_mut(req.id()).await?;
            let id = container.id().await;
            let exec_id_opt = req.exec_id().as_option();
            let (pid, exit_status, exited_at) = container.delete(exec_id_opt).await?;
            self.factory.cleanup(&self.namespace, &container).await?;
            (id, pid, exit_status, exited_at)
        };

        if req.exec_id().is_empty() {
            self.containers.write().await.remove(req.id());
        }

        let ts = convert_to_timestamp(exited_at);
        self.send_event(TaskDelete {
            container_id: id,
            pid: pid as u32,
            exit_status: exit_status as u32,
            exited_at: Some(ts.clone()).into(),
            ..Default::default()
        })
        .await;

        let mut resp = DeleteResponse::new();
        resp.set_exited_at(ts);
        resp.set_pid(pid as u32);
        resp.set_exit_status(exit_status as u32);
        info!(
            "Delete request for {} {} returns {:?}",
            req.id(),
            req.exec_id(),
            resp
        );
        Ok(resp)
    }

    async fn pids(&self, _ctx: &TtrpcContext, req: PidsRequest) -> TtrpcResult<PidsResponse> {
        debug!("Pids request for {:?}", req);
        let processes = self.container(req.id()).await?.all_processes().await?;
        debug!("Pids request for {:?} returns successfully", req);
        Ok(PidsResponse {
            processes,
            ..Default::default()
        })
    }

    async fn pause(&self, _ctx: &TtrpcContext, req: PauseRequest) -> TtrpcResult<Empty> {
        info!("pause request for {:?}", req);
        self.container_mut(req.id()).await?.pause().await?;
        self.send_event(TaskPaused {
            container_id: req.id.to_string(),
            ..Default::default()
        })
        .await;
        info!("pause request for {:?} returns successfully", req);
        Ok(Empty::new())
    }

    async fn resume(&self, _ctx: &TtrpcContext, req: ResumeRequest) -> TtrpcResult<Empty> {
        info!("resume request for {:?}", req);
        self.container_mut(req.id()).await?.resume().await?;
        self.send_event(TaskResumed {
            container_id: req.id.to_string(),
            ..Default::default()
        })
        .await;
        info!("resume request for {:?} returns successfully", req);
        Ok(Empty::new())
    }

    async fn kill(&self, _ctx: &TtrpcContext, req: KillRequest) -> TtrpcResult<Empty> {
        info!("Kill request for {:?}", req);
        self.container_mut(req.id())
            .await?
            .kill(req.exec_id().as_option(), req.signal, req.all)
            .await?;
        info!("Kill request for {:?} returns successfully", req);
        Ok(Empty::new())
    }

    async fn exec(&self, _ctx: &TtrpcContext, req: ExecProcessRequest) -> TtrpcResult<Empty> {
        info!("Exec request for {:?}", req);
        let exec_id = req.exec_id().to_string();

        let container_id = {
            let mut container = self.container_mut(req.id()).await?;
            container.exec(req).await?;
            container.id().await
        };

        self.send_event(TaskExecAdded {
            container_id,
            exec_id,
            ..Default::default()
        })
        .await;

        Ok(Empty::new())
    }

    async fn resize_pty(&self, _ctx: &TtrpcContext, req: ResizePtyRequest) -> TtrpcResult<Empty> {
        debug!(
            "Resize pty request for container {}, exec_id: {}",
            &req.id, &req.exec_id
        );
        self.container_mut(req.id())
            .await?
            .resize_pty(req.exec_id().as_option(), req.height, req.width)
            .await?;
        Ok(Empty::new())
    }

    async fn close_io(&self, _ctx: &TtrpcContext, req: CloseIORequest) -> TtrpcResult<Empty> {
        self.container_mut(req.id())
            .await?
            .close_io(req.exec_id().as_option())
            .await?;
        Ok(Empty::new())
    }

    async fn update(&self, _ctx: &TtrpcContext, mut req: UpdateTaskRequest) -> TtrpcResult<Empty> {
        debug!("Update request for id {:?}", req.id);

        let id = req.take_id();

        let data = req
            .resources
            .into_option()
            .map(|r| r.value)
            .unwrap_or_default();

        let resources: LinuxResources = serde_json::from_slice(&data).map_err(|e| {
            ttrpc::Error::RpcStatus(ttrpc::get_status(
                ttrpc::Code::INVALID_ARGUMENT,
                format!("failed to parse resource spec: {}", e),
            ))
        })?;
        debug!("Update resource is {:?}", resources);
        self.container_mut(&id).await?.update(&resources).await?;
        Ok(Empty::new())
    }

    async fn wait(&self, _ctx: &TtrpcContext, req: WaitRequest) -> TtrpcResult<WaitResponse> {
        info!("Wait request for {:?}", req);
        let exec_id = req.exec_id.as_str().as_option();
        let wait_rx = {
            let mut container = self.container_mut(req.id()).await?;
            let state = container.state(exec_id).await?;
            if state.status() != Status::RUNNING && state.status() != Status::CREATED {
                let mut resp = WaitResponse::new();
                resp.exit_status = state.exit_status;
                resp.exited_at = state.exited_at;
                info!("Wait request for {:?} returns {:?}", req, &resp);
                return Ok(resp);
            }
            container.wait_channel(req.exec_id().as_option()).await?
        };

        wait_rx.await.unwrap_or_default();
        // get lock again.
        let (_, code, exited_at) = self
            .container(req.id())
            .await?
            .get_exit_info(exec_id)
            .await?;
        let mut resp = WaitResponse::new();
        resp.set_exit_status(code as u32);
        let ts = convert_to_timestamp(exited_at);
        resp.set_exited_at(ts);
        info!("Wait request for {:?} returns {:?}", req, &resp);
        Ok(resp)
    }

    async fn stats(&self, _ctx: &TtrpcContext, req: StatsRequest) -> TtrpcResult<StatsResponse> {
        debug!("Stats request for {:?}", req);
        let stats = self.container(req.id()).await?.stats().await?;
        let mut resp = StatsResponse::new();
        resp.set_stats(convert_to_any(Box::new(stats))?);
        Ok(resp)
    }

    async fn connect(
        &self,
        _ctx: &TtrpcContext,
        req: ConnectRequest,
    ) -> TtrpcResult<ConnectResponse> {
        info!("Connect request for {:?}", req);

        let pid = if let Ok(container) = self.container(req.id()).await {
            container.pid().await as u32
        } else {
            0
        };

        Ok(ConnectResponse {
            shim_pid: std::process::id(),
            task_pid: pid,
            ..Default::default()
        })
    }

    async fn shutdown(&self, _ctx: &TtrpcContext, _req: ShutdownRequest) -> TtrpcResult<Empty> {
        debug!("Shutdown request");
        let containers = self.containers.read().await;
        if !containers.is_empty() {
            return Ok(Empty::new());
        }
        self.exit.signal();
        Ok(Empty::default())
    }
}

/// Behaviour of the ttrpc `Task` service, driven over a fake OCI runtime.
///
/// Everything here goes through the `Task` trait, which is the API containerd
/// itself calls. That keeps the tests about observable behaviour rather than the
/// types underneath, so they stay valid as those types change.
#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        os::unix::process::ExitStatusExt,
        process::ExitStatus,
        sync::{
            atomic::{AtomicI32, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use containerd_shim::{
        api::{
            CloseIORequest, ConnectRequest, CreateTaskRequest, DeleteRequest, ExecProcessRequest,
            KillRequest, Options, PidsRequest, ResizePtyRequest, ShutdownRequest, StartRequest,
            StateRequest, StatsRequest, Status, WaitRequest,
        },
        asynchronous::{monitor::Subscription, ExitSignal},
        monitor::{ExitEvent, Subject},
        protos::{
            events::task::{TaskDelete, TaskExit},
            protobuf::{well_known_types::any::Any, Message, MessageDyn, MessageField},
            shim::oci::ProcessDetails,
            shim_async::Task,
            ttrpc,
            ttrpc::{r#async::TtrpcContext, Code},
        },
    };
    use oci_spec::runtime::{Process, Spec};
    use runc::{Command, Spawner};
    use tempfile::TempDir;
    use tokio::sync::mpsc::{
        channel, error::TryRecvError, unbounded_channel, Receiver, UnboundedSender,
    };

    use crate::{
        runc::{RuncContainer, RuncFactory},
        service::process_exits,
        task::TaskService,
    };

    // ===========================================================================
    // Harness
    // ===========================================================================

    /// The concrete `TaskService` under test, aliased so that collapsing
    /// `TaskService<F, C>` touches this line rather than the fixture.
    type Shim = TaskService<RuncFactory, RuncContainer>;

    /// Hands out pids that no real process can own.
    ///
    /// Starting above `pid_max` means that if a code path ever issues a real
    /// `kill(2)` against one of these it fails with `ESRCH` rather than signalling
    /// something real. Pids stay unique per test so failures name one container.
    fn next_fake_pid() -> i32 {
        static NEXT: AtomicI32 = AtomicI32::new(0x4000_0000);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    #[derive(Debug, Default)]
    struct FakeState {
        /// Full argv of every invocation, in order.
        calls: Vec<Vec<String>>,
        /// JSON payload for `ps --format=json`.
        ps_pids: Vec<usize>,
        /// stderr to fail with, keyed by subcommand.
        failures: HashMap<String, String>,
    }

    /// A [`Spawner`] that records invocations and simulates the side effects the
    /// shim depends on, so the Task API can be driven without a real OCI runtime.
    ///
    /// Every `Runc` method funnels through a single `Spawner::execute` call, so this
    /// intercepts all of them.
    #[derive(Debug, Default)]
    struct FakeRunc {
        state: Mutex<FakeState>,
    }

    impl FakeRunc {
        /// Makes `subcommand` exit non-zero, writing `stderr` on the runtime stderr.
        fn fail(&self, subcommand: &str, stderr: &str) {
            self.state
                .lock()
                .unwrap()
                .failures
                .insert(subcommand.to_string(), stderr.to_string());
        }

        /// Sets what `runc ps` reports.
        fn set_ps_pids(&self, pids: Vec<usize>) {
            self.state.lock().unwrap().ps_pids = pids;
        }

        /// Argv of every invocation of `subcommand`, from the subcommand onwards.
        fn calls_for(&self, subcommand: &str) -> Vec<Vec<String>> {
            self.state
                .lock()
                .unwrap()
                .calls
                .iter()
                .map(|argv| tail_from_subcommand(argv))
                .filter(|tail| tail[0] == subcommand)
                .map(<[String]>::to_vec)
                .collect()
        }
    }

    /// Splits an argv at the runc subcommand, returning the subcommand and
    /// everything after it.
    ///
    /// `Runc::launch_io` builds `[global_args, command_args].concat()`, and the
    /// global set is closed: `--root`, `--log` and `--log-format` each take a
    /// separate value, `--debug` and `--systemd-cgroup` take none, and
    /// `--rootless=` is a single token. Decoding that prefix is exact, where
    /// scanning for a known verb would silently mis-parse an argv whose shape
    /// changed.
    fn tail_from_subcommand(argv: &[String]) -> &[String] {
        let mut i = 0;
        while let Some(arg) = argv.get(i) {
            match arg.as_str() {
                "--root" | "--log" | "--log-format" => i += 2,
                a if a.starts_with('-') => i += 1,
                _ => return &argv[i..],
            }
        }
        panic!("fake runc: no subcommand in argv {:?}", argv);
    }

    /// Value following `flag` in an argv, if present.
    fn flag_value(argv: &[String], flag: &str) -> Option<String> {
        let i = argv.iter().position(|a| a == flag)?;
        argv.get(i + 1).cloned()
    }

    #[async_trait]
    impl Spawner for FakeRunc {
        async fn execute(&self, cmd: Command) -> runc::Result<(ExitStatus, u32, String, String)> {
            let argv: Vec<String> = cmd
                .as_std()
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let subcommand = tail_from_subcommand(&argv)[0].clone();
            let pid_file = flag_value(&argv, "--pid-file");

            let failure = {
                let mut state = self.state.lock().unwrap();
                let failure = state.failures.get(&subcommand).cloned();
                state.calls.push(argv);
                failure
            };
            let pid = next_fake_pid();

            if let Some(stderr) = failure {
                // Exit code 1. `launch_io` turns any non-success status into
                // `Error::CommandFailed { status, stdout, stderr }`, which is what
                // `runtime_error` and `check_kill_error` consume.
                return Ok((
                    ExitStatus::from_raw(1 << 8),
                    pid as u32,
                    String::new(),
                    stderr,
                ));
            }

            // Real runc writes the container pid here; the shim reads it back
            // immediately after create and exec.
            if let Some(path) = pid_file {
                std::fs::write(&path, pid.to_string())
                    .unwrap_or_else(|e| panic!("fake runc: write pid file {}: {}", path, e));
            }

            let stdout = if subcommand == "ps" {
                serde_json::to_string(&self.state.lock().unwrap().ps_pids).unwrap()
            } else {
                String::new()
            };

            Ok((ExitStatus::from_raw(0), pid as u32, stdout, String::new()))
        }
    }

    /// A `TaskService` backed by a fake runtime and a throwaway bundle directory.
    struct TestShim {
        task: Arc<Shim>,
        runc: Arc<FakeRunc>,
        exit: Arc<ExitSignal>,
        /// Feeds process-exit events to this fixture's `process_exits` pump.
        ///
        /// The shim's own exit monitor is a process-global singleton. Handing the
        /// pump a private channel instead keeps fixtures fully isolated from each
        /// other, and lets the pump task end on its own when the fixture drops and
        /// this sender goes with it.
        exits: UnboundedSender<ExitEvent>,
        events: Receiver<(String, Box<dyn MessageDyn>)>,
        bundle: TempDir,
    }

    impl TestShim {
        /// Builds the service and starts the exit pump, mirroring what
        /// `Service::create_task_service` wires up in production.
        async fn new() -> Self {
            let bundle = tempfile::tempdir().expect("create bundle dir");

            // `should_kill_all_on_exit` reads this when the init process exits;
            // without it that read fails and the code takes a log-and-continue path.
            std::fs::write(
                bundle.path().join("config.json"),
                serde_json::to_string(&Spec::default()).unwrap(),
            )
            .expect("write config.json");

            let runc = Arc::new(FakeRunc::default());
            let (tx, events) = channel(128);
            let exit = Arc::new(ExitSignal::default());

            let mut task = Shim::new("runc-shim-test", exit.clone(), tx.clone());
            task.factory.spawner = runc.clone();
            let task = Arc::new(task);

            // A private stand-in for the global pid monitor. The id is never
            // registered anywhere; `process_exits` only passes it to an unsubscribe
            // call on its way out, and ids issued by the real monitor start at 0.
            let (exits, rx) = unbounded_channel();
            process_exits(Subscription { id: -1, rx }, &task, tx).await;

            Self {
                task,
                runc,
                exit,
                exits,
                events,
                bundle,
            }
        }

        fn bundle(&self) -> String {
            self.bundle.path().to_string_lossy().into_owned()
        }

        /// A create request for `id` against this shim's bundle.
        ///
        /// stdio is left empty on purpose: that makes `Stdio::is_null()` true, so
        /// the shim uses `NullIo` and skips all fifo and console plumbing.
        fn create_request(&self, id: &str) -> CreateTaskRequest {
            let mut opts = Options::new();
            // `GlobalOpts::build` resolves the runtime through `binary_path`,
            // which needs a real file on disk even though FakeRunc never
            // executes it — and which returns None when PATH is unset, absolute
            // path or not. So these tests require PATH to be present.
            opts.binary_name = std::env::current_exe()
                .expect("current_exe")
                .to_string_lossy()
                .into_owned();
            // Keep the runc state root inside the bundle so nothing reaches for
            // /run/containerd.
            opts.root = self
                .bundle
                .path()
                .join("runc-root")
                .to_string_lossy()
                .into_owned();

            let mut any = Any::new();
            any.type_url = "containerd.runc.v1.Options".to_string();
            any.value = opts.write_to_bytes().expect("encode options");

            CreateTaskRequest {
                id: id.to_string(),
                bundle: self.bundle(),
                options: MessageField::some(any),
                ..Default::default()
            }
        }

        /// Reports that `pid` exited, and waits for the shim to act on it.
        ///
        /// The exit pump runs as a spawned task, so acting and asserting in the
        /// same breath can outrun it. The published `TaskExit` is the sync point.
        async fn exit_process(&mut self, pid: i32, code: i32) -> TaskExit {
            self.exits
                .send(ExitEvent {
                    subject: Subject::Pid(pid),
                    exit_code: code,
                })
                .expect("exit pump should still be running");
            self.await_event::<TaskExit>("/tasks/exit").await
        }

        /// Creates and starts a container, returning its init pid.
        async fn start_container(&self, id: &str) -> i32 {
            self.task
                .create(&ctx(), self.create_request(id))
                .await
                .expect("create container");
            let resp = self
                .task
                .start(&ctx(), start_request(id, ""))
                .await
                .expect("start container");
            resp.pid as i32
        }

        /// Creates and starts an exec process, returning its pid.
        async fn start_exec(&self, id: &str, exec_id: &str) -> i32 {
            self.task
                .exec(&ctx(), exec_request(id, exec_id))
                .await
                .expect("exec");
            let resp = self
                .task
                .start(&ctx(), start_request(id, exec_id))
                .await
                .expect("start exec");
            resp.pid as i32
        }

        /// Topics of every event published so far, in order.
        fn event_topics(&mut self) -> Vec<String> {
            let mut out = Vec::new();
            loop {
                match self.events.try_recv() {
                    Ok((topic, _)) => out.push(topic),
                    Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return out,
                }
            }
        }

        /// Waits for an event on `topic`, decoded as `T`.
        ///
        /// Exit events are published from a spawned task, so a test that acts and
        /// then asserts immediately can outrun the publisher.
        async fn await_event<T: Message>(&mut self, topic: &str) -> T {
            loop {
                let (got, msg) = tokio::time::timeout(Duration::from_secs(5), self.events.recv())
                    .await
                    .unwrap_or_else(|_| panic!("timed out waiting for {}", topic))
                    .expect("event channel closed");
                if got == topic {
                    return T::parse_from_bytes(&msg.write_to_bytes_dyn().expect("encode event"))
                        .expect("decode event");
                }
            }
        }
    }

    /// A request context. The shim ignores it, but the generated trait requires one.
    fn ctx() -> TtrpcContext {
        TtrpcContext {
            mh: Default::default(),
            metadata: Default::default(),
            timeout_nano: 0,
        }
    }

    fn start_request(id: &str, exec_id: &str) -> StartRequest {
        StartRequest {
            id: id.to_string(),
            exec_id: exec_id.to_string(),
            ..Default::default()
        }
    }

    fn state_request(id: &str, exec_id: &str) -> StateRequest {
        StateRequest {
            id: id.to_string(),
            exec_id: exec_id.to_string(),
            ..Default::default()
        }
    }

    fn delete_request(id: &str, exec_id: &str) -> DeleteRequest {
        DeleteRequest {
            id: id.to_string(),
            exec_id: exec_id.to_string(),
            ..Default::default()
        }
    }

    fn wait_request(id: &str, exec_id: &str) -> WaitRequest {
        WaitRequest {
            id: id.to_string(),
            exec_id: exec_id.to_string(),
            ..Default::default()
        }
    }

    fn kill_request(id: &str, signal: u32, all: bool) -> KillRequest {
        KillRequest {
            id: id.to_string(),
            signal,
            all,
            ..Default::default()
        }
    }

    fn pids_request(id: &str) -> PidsRequest {
        PidsRequest {
            id: id.to_string(),
            ..Default::default()
        }
    }

    fn stats_request(id: &str) -> StatsRequest {
        StatsRequest {
            id: id.to_string(),
            ..Default::default()
        }
    }

    fn connect_request(id: &str) -> ConnectRequest {
        ConnectRequest {
            id: id.to_string(),
            ..Default::default()
        }
    }

    /// An exec request carrying a default process spec.
    fn exec_request(id: &str, exec_id: &str) -> ExecProcessRequest {
        let mut any = Any::new();
        any.type_url = "types.containerd.io/opencontainers/runtime-spec/1/Process".to_string();
        any.value = serde_json::to_vec(&Process::default()).expect("encode process spec");

        ExecProcessRequest {
            id: id.to_string(),
            exec_id: exec_id.to_string(),
            spec: MessageField::some(any),
            ..Default::default()
        }
    }

    /// The ttrpc status code carried by an error, if it has one.
    fn code_of(err: &ttrpc::Error) -> Option<Code> {
        match err {
            ttrpc::Error::RpcStatus(s) => Some(s.code()),
            _ => None,
        }
    }

    // ===========================================================================
    // Dispatch between the init process and execs
    // ===========================================================================

    #[tokio::test]
    async fn state_dispatch() {
        let shim = TestShim::new().await;
        let init_pid = shim.start_container("dispatch").await;
        let exec_pid = shim.start_exec("dispatch", "e1").await;

        // Fixture precondition, not a claim about the shim: the pid assertions
        // below can only tell the two processes apart if the fake runtime handed
        // out different pids for them.
        assert_ne!(init_pid, exec_pid, "fake runtime reused a pid");

        // Empty exec_id addresses the init process.
        let init = shim
            .task
            .state(&ctx(), state_request("dispatch", ""))
            .await
            .expect("state of init");
        assert_eq!(init.id, "dispatch");
        assert_eq!(init.pid, init_pid as u32);
        assert_eq!(init.status(), Status::RUNNING);
        assert_eq!(init.bundle, shim.bundle());

        // A known exec_id addresses that exec.
        let exec = shim
            .task
            .state(&ctx(), state_request("dispatch", "e1"))
            .await
            .expect("state of exec");
        assert_eq!(exec.id, "e1");
        assert_eq!(exec.pid, exec_pid as u32);
        assert_eq!(exec.status(), Status::RUNNING);

        // An unknown exec_id is not found.
        let err = shim
            .task
            .state(&ctx(), state_request("dispatch", "nope"))
            .await
            .expect_err("unknown exec should not resolve");
        assert_eq!(code_of(&err), Some(Code::NOT_FOUND));
    }

    #[tokio::test]
    async fn unknown_container() {
        let shim = TestShim::new().await;

        // Every method that takes a container id rejects one it does not know.
        let errs = [
            (
                "state",
                shim.task
                    .state(&ctx(), state_request("ghost", ""))
                    .await
                    .err(),
            ),
            (
                "kill",
                shim.task
                    .kill(&ctx(), kill_request("ghost", 9, false))
                    .await
                    .err(),
            ),
            (
                "delete",
                shim.task
                    .delete(&ctx(), delete_request("ghost", ""))
                    .await
                    .err(),
            ),
            (
                "pids",
                shim.task.pids(&ctx(), pids_request("ghost")).await.err(),
            ),
            (
                "exec",
                shim.task
                    .exec(&ctx(), exec_request("ghost", "e1"))
                    .await
                    .err(),
            ),
            (
                "stats",
                shim.task.stats(&ctx(), stats_request("ghost")).await.err(),
            ),
        ];

        for (method, err) in errs {
            let err =
                err.unwrap_or_else(|| panic!("{} on an unknown container should fail", method));
            assert_eq!(code_of(&err), Some(Code::NOT_FOUND), "{}: {}", method, err);
        }
    }

    // ===========================================================================
    // Exit: waiters, retained metadata, published events
    // ===========================================================================

    /// Concurrent waiters are all served the task's exit status.
    ///
    /// The pending check below rules out a waiter that returned early, but it
    /// cannot prove one registered: a spawned task that has not been polled yet
    /// is pending too, and would take the same short-circuit `wait_after_exit`
    /// covers. Proving registration means reading `wait_chan_tx`, which is the
    /// implementation detail these tests deliberately stay off. What is pinned
    /// here is that no waiter is dropped or served a different result.
    #[tokio::test]
    async fn concurrent_waiters() {
        let mut shim = TestShim::new().await;
        let pid = shim.start_container("waiters").await;

        let mut waiters: Vec<_> = (0..3)
            .map(|_| {
                let task = shim.task.clone();
                tokio::spawn(async move { task.wait(&ctx(), wait_request("waiters", "")).await })
            })
            .collect();

        for waiter in &mut waiters {
            assert!(
                tokio::time::timeout(Duration::from_millis(50), waiter)
                    .await
                    .is_err(),
                "a waiter returned before the task exited"
            );
        }

        shim.exit_process(pid, 7).await;

        for waiter in waiters {
            // Bounded so a regression that drops a waiter fails here instead of
            // hanging CI until the job timeout.
            let resp = tokio::time::timeout(Duration::from_secs(5), waiter)
                .await
                .expect("a waiter was never woken after the task exited")
                .expect("waiter task panicked")
                .expect("wait should succeed");
            assert_eq!(resp.exit_status, 7);
            assert!(
                resp.exited_at.is_some(),
                "an exited process must carry a timestamp"
            );
        }
    }

    #[tokio::test]
    async fn wait_after_exit() {
        let mut shim = TestShim::new().await;
        let pid = shim.start_container("late-wait").await;

        shim.exit_process(pid, 3).await;

        // Exit metadata is retained, not consumed: a wait issued afterwards still
        // reports it, and reports it repeatedly.
        for _ in 0..2 {
            let resp = shim
                .task
                .wait(&ctx(), wait_request("late-wait", ""))
                .await
                .expect("wait after exit");
            assert_eq!(resp.exit_status, 3);
            assert!(resp.exited_at.is_some());
        }
    }

    #[tokio::test]
    async fn state_after_exit() {
        let mut shim = TestShim::new().await;
        let pid = shim.start_container("stopped").await;

        shim.exit_process(pid, 42).await;

        let resp = shim
            .task
            .state(&ctx(), state_request("stopped", ""))
            .await
            .expect("state after exit");
        assert_eq!(resp.status(), Status::STOPPED);
        assert_eq!(resp.exit_status, 42);
        assert!(resp.exited_at.is_some());
    }

    #[tokio::test]
    async fn exit_event() {
        let mut shim = TestShim::new().await;
        let pid = shim.start_container("exit-event").await;

        let event = shim.exit_process(pid, 9).await;
        assert_eq!(event.container_id, "exit-event");
        assert_eq!(event.id, "exit-event");
        assert_eq!(event.pid, pid as u32);
        assert_eq!(event.exit_status, 9);
        assert!(event.exited_at.is_some());
    }

    #[tokio::test]
    async fn exec_exit_event() {
        let mut shim = TestShim::new().await;
        shim.start_container("exec-exit").await;
        let exec_pid = shim.start_exec("exec-exit", "e1").await;

        let event = shim.exit_process(exec_pid, 5).await;
        assert_eq!(event.container_id, "exec-exit");
        assert_eq!(
            event.id, "e1",
            "the event identifies the exec, not the task"
        );
        assert_eq!(event.pid, exec_pid as u32);
        assert_eq!(event.exit_status, 5);
    }

    #[tokio::test]
    async fn start_after_exit() {
        let mut shim = TestShim::new().await;
        let pid = shim.start_container("dead").await;
        shim.exit_process(pid, 0).await;

        let err = shim
            .task
            .start(&ctx(), start_request("dead", ""))
            .await
            .expect_err("starting into a dead container should fail");
        assert_eq!(code_of(&err), Some(Code::FAILED_PRECONDITION));
    }

    // ===========================================================================
    // Delete
    // ===========================================================================

    #[tokio::test]
    async fn delete_exec() {
        let mut shim = TestShim::new().await;
        shim.start_container("del-exec").await;
        let exec_pid = shim.start_exec("del-exec", "e1").await;
        shim.exit_process(exec_pid, 4).await;

        let resp = shim
            .task
            .delete(&ctx(), delete_request("del-exec", "e1"))
            .await
            .expect("delete exec");
        assert_eq!(resp.pid, exec_pid as u32);
        assert_eq!(resp.exit_status, 4);

        // The exec is gone...
        let err = shim
            .task
            .state(&ctx(), state_request("del-exec", "e1"))
            .await
            .expect_err("deleted exec should not resolve");
        assert_eq!(code_of(&err), Some(Code::NOT_FOUND));

        // ...but the container is not.
        shim.task
            .state(&ctx(), state_request("del-exec", ""))
            .await
            .expect("container should survive deleting one of its execs");
    }

    #[tokio::test]
    async fn delete_container() {
        let mut shim = TestShim::new().await;
        let pid = shim.start_container("del-task").await;
        shim.exit_process(pid, 0).await;

        let resp = shim
            .task
            .delete(&ctx(), delete_request("del-task", ""))
            .await
            .expect("delete container");
        assert_eq!(resp.pid, pid as u32);

        let err = shim
            .task
            .state(&ctx(), state_request("del-task", ""))
            .await
            .expect_err("deleted container should not resolve");
        assert_eq!(code_of(&err), Some(Code::NOT_FOUND));
    }

    #[tokio::test]
    async fn delete_event() {
        let mut shim = TestShim::new().await;
        let pid = shim.start_container("del-event").await;
        shim.exit_process(pid, 6).await;

        shim.task
            .delete(&ctx(), delete_request("del-event", ""))
            .await
            .expect("delete");

        let event = shim.await_event::<TaskDelete>("/tasks/delete").await;
        assert_eq!(event.container_id, "del-event");
        assert_eq!(event.pid, pid as u32);
        assert_eq!(event.exit_status, 6);
    }

    // ===========================================================================
    // Published events for the happy path
    // ===========================================================================

    #[tokio::test]
    async fn lifecycle_events() {
        let mut shim = TestShim::new().await;
        shim.start_container("lifecycle").await;
        shim.start_exec("lifecycle", "e1").await;

        assert_eq!(
            shim.event_topics(),
            [
                "/tasks/create",
                "/tasks/start",
                "/tasks/exec-added",
                "/tasks/exec-started",
            ]
        );
    }

    // ===========================================================================
    // Errors coming back from the runtime
    // ===========================================================================

    #[tokio::test]
    async fn create_failure() {
        let shim = TestShim::new().await;

        // The shim reads the last error line out of the runtime log to explain a
        // failed create.
        std::fs::write(
            std::path::Path::new(&shim.bundle()).join(crate::common::LOG_JSON_FILE),
            "{\"level\":\"info\",\"msg\":\"hello\",\"time\":\"2024-01-01\"}\n\
             {\"level\":\"error\",\"msg\":\"rootfs is not a directory\",\"time\":\"2024-01-01\"}\n",
        )
        .expect("seed runtime log");
        shim.runc.fail("create", "exit status 1");

        let err = shim
            .task
            .create(&ctx(), shim.create_request("bad-create"))
            .await
            .expect_err("create should fail");
        assert!(
            err.to_string().contains("rootfs is not a directory"),
            "expected the runtime log error in {:?}",
            err.to_string()
        );
    }

    #[tokio::test]
    async fn kill_error_mapping() {
        let shim = TestShim::new().await;
        shim.start_container("kill-err").await;

        // A failed kill leaves the container untouched, so one fixture covers
        // every phrasing runc might report.
        for stderr in [
            "process already finished",
            "container not running",
            "no such process",
            "container does not exist",
        ] {
            shim.runc.fail("kill", stderr);

            let err = shim
                .task
                .kill(&ctx(), kill_request("kill-err", 9, false))
                .await
                .err()
                .unwrap_or_else(|| panic!("kill should fail for {:?}", stderr));
            assert_eq!(
                code_of(&err),
                Some(Code::NOT_FOUND),
                "runtime said {:?}, shim reported {}",
                stderr,
                err
            );
        }
    }

    #[tokio::test]
    async fn kill_args() {
        let shim = TestShim::new().await;
        shim.start_container("kill-args").await;

        shim.task
            .kill(&ctx(), kill_request("kill-args", 15, true))
            .await
            .expect("kill");

        let calls = shim.runc.calls_for("kill");
        assert_eq!(calls.len(), 1, "expected exactly one runc kill");
        assert_eq!(calls[0], ["kill", "--all", "kill-args", "15"]);
    }

    // ===========================================================================
    // Pids, connect, shutdown, stdio
    // ===========================================================================

    #[tokio::test]
    async fn pids_exec_details() {
        let shim = TestShim::new().await;
        let init_pid = shim.start_container("pids").await;
        let exec_pid = shim.start_exec("pids", "e1").await;
        shim.runc
            .set_ps_pids(vec![init_pid as usize, exec_pid as usize]);

        let resp = shim
            .task
            .pids(&ctx(), pids_request("pids"))
            .await
            .expect("pids");
        assert_eq!(resp.processes.len(), 2);

        let init = resp
            .processes
            .iter()
            .find(|p| p.pid == init_pid as u32)
            .expect("init pid reported");
        assert!(
            init.info.is_none(),
            "the init process carries no exec details"
        );

        let exec = resp
            .processes
            .iter()
            .find(|p| p.pid == exec_pid as u32)
            .expect("exec pid reported");
        let info = exec.info.as_ref().expect("exec details attached");
        let details =
            ProcessDetails::parse_from_bytes(&info.value).expect("decode process details");
        assert_eq!(details.exec_id, "e1");
    }

    #[tokio::test]
    async fn connect() {
        let shim = TestShim::new().await;
        let pid = shim.start_container("connect").await;

        let resp = shim
            .task
            .connect(&ctx(), connect_request("connect"))
            .await
            .expect("connect");
        assert_eq!(resp.shim_pid, std::process::id());
        assert_eq!(resp.task_pid, pid as u32);

        // An unknown container reports no task pid rather than failing.
        let resp = shim
            .task
            .connect(&ctx(), connect_request("ghost"))
            .await
            .expect("connect to unknown container");
        assert_eq!(resp.task_pid, 0);
    }

    #[tokio::test]
    async fn shutdown() {
        let mut shim = TestShim::new().await;
        let pid = shim.start_container("shutdown").await;

        shim.task
            .shutdown(&ctx(), ShutdownRequest::default())
            .await
            .expect("shutdown with a live container");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), shim.exit.wait())
                .await
                .is_err(),
            "shutdown must not signal exit while a container is still held"
        );

        shim.exit_process(pid, 0).await;
        shim.task
            .delete(&ctx(), delete_request("shutdown", ""))
            .await
            .expect("delete");

        shim.task
            .shutdown(&ctx(), ShutdownRequest::default())
            .await
            .expect("shutdown when empty");
        tokio::time::timeout(Duration::from_secs(5), shim.exit.wait())
            .await
            .expect("shutdown should signal exit once empty");
    }

    #[tokio::test]
    async fn resize_and_close_io() {
        let shim = TestShim::new().await;
        shim.start_container("stdio").await;
        shim.start_exec("stdio", "e1").await;

        for exec_id in ["", "e1"] {
            shim.task
                .resize_pty(
                    &ctx(),
                    ResizePtyRequest {
                        id: "stdio".to_string(),
                        exec_id: exec_id.to_string(),
                        width: 80,
                        height: 24,
                        ..Default::default()
                    },
                )
                .await
                .unwrap_or_else(|e| panic!("resize_pty({:?}): {}", exec_id, e));

            // `stdin` is left unset on purpose: `TaskService::close_io` never
            // reads the flag and always closes stdin, so setting it here would
            // imply coverage that does not exist.
            shim.task
                .close_io(
                    &ctx(),
                    CloseIORequest {
                        id: "stdio".to_string(),
                        exec_id: exec_id.to_string(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap_or_else(|e| panic!("close_io({:?}): {}", exec_id, e));
        }

        let err = shim
            .task
            .resize_pty(
                &ctx(),
                ResizePtyRequest {
                    id: "stdio".to_string(),
                    exec_id: "nope".to_string(),
                    width: 80,
                    height: 24,
                    ..Default::default()
                },
            )
            .await
            .expect_err("resizing an unknown exec should fail");
        assert_eq!(code_of(&err), Some(Code::NOT_FOUND));
    }

    /// The fake runtime hands out pids that own no cgroup, so metrics collection
    /// has nothing to read. It must say so rather than panic or invent numbers.
    #[tokio::test]
    async fn stats_no_cgroup() {
        let shim = TestShim::new().await;
        shim.start_container("stats").await;

        let err = shim
            .task
            .stats(&ctx(), stats_request("stats"))
            .await
            .expect_err("a container with no cgroup has no metrics to report");

        #[cfg(target_os = "linux")]
        assert!(
            err.to_string().contains("cgroup"),
            "expected a cgroup failure, got {}",
            err
        );
        #[cfg(not(target_os = "linux"))]
        assert!(
            err.to_string().contains("Unimplemented method"),
            "stats is not supported off Linux, got {}",
            err
        );
    }

    // ===========================================================================
    // Pause and resume are Linux-only
    // ===========================================================================

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn pause_resume() {
        use containerd_shim::api::{PauseRequest, ResumeRequest};

        let shim = TestShim::new().await;
        shim.start_container("paused").await;
        shim.start_exec("paused", "e1").await;

        shim.task
            .pause(
                &ctx(),
                PauseRequest {
                    id: "paused".to_string(),
                    ..Default::default()
                },
            )
            .await
            .expect("pause");

        // A paused task projects its status onto every exec inside it, even though
        // the exec process itself was never touched.
        for exec_id in ["", "e1"] {
            let resp = shim
                .task
                .state(&ctx(), state_request("paused", exec_id))
                .await
                .unwrap_or_else(|e| panic!("state({:?}): {}", exec_id, e));
            assert_eq!(
                resp.status(),
                Status::PAUSED,
                "exec_id {:?} should report PAUSED",
                exec_id
            );
        }

        shim.task
            .resume(
                &ctx(),
                ResumeRequest {
                    id: "paused".to_string(),
                    ..Default::default()
                },
            )
            .await
            .expect("resume");

        for exec_id in ["", "e1"] {
            let resp = shim
                .task
                .state(&ctx(), state_request("paused", exec_id))
                .await
                .unwrap_or_else(|e| panic!("state({:?}): {}", exec_id, e));
            assert_eq!(resp.status(), Status::RUNNING);
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn pause_unsupported() {
        use containerd_shim::api::PauseRequest;

        let shim = TestShim::new().await;
        shim.start_container("no-pause").await;

        let err = shim
            .task
            .pause(
                &ctx(),
                PauseRequest {
                    id: "no-pause".to_string(),
                    ..Default::default()
                },
            )
            .await
            .expect_err("pause is not supported off Linux");

        // Not `Code::UNIMPLEMENTED`: `Error::Unimplemented` has no ttrpc mapping
        // and reaches containerd as an opaque error. `unimplemented_no_code`
        // below pins that gap on every platform.
        assert_eq!(code_of(&err), None, "got {}", err);
        assert!(
            err.to_string().contains("Unimplemented method"),
            "got {}",
            err
        );
    }

    // =======================================================================
    // How shim errors reach containerd
    // =======================================================================

    fn ttrpc_code(err: containerd_shim::Error) -> Option<Code> {
        code_of(&err.into())
    }

    #[test]
    fn error_codes() {
        use containerd_shim::Error;

        assert_eq!(
            ttrpc_code(Error::InvalidArgument("x".to_string())),
            Some(Code::INVALID_ARGUMENT)
        );
        assert_eq!(
            ttrpc_code(Error::NotFoundError("x".to_string())),
            Some(Code::NOT_FOUND)
        );
        assert_eq!(
            ttrpc_code(Error::FailedPreconditionError("x".to_string())),
            Some(Code::FAILED_PRECONDITION)
        );
    }

    /// `Error::Unimplemented` is not mapped to a ttrpc status, so every method
    /// the shim does not support on the current platform (`pause`, `resume`,
    /// `update` and `stats` off Linux; `update`/`stats`/`ps`/`pause`/`resume` on
    /// an exec) reaches containerd as an opaque error rather than
    /// `Code::UNIMPLEMENTED`.
    ///
    /// Pinned so that closing the gap is a deliberate change, not an accident.
    #[test]
    fn unimplemented_no_code() {
        let err = containerd_shim::Error::Unimplemented("pause".to_string());
        assert!(err.to_string().contains("Unimplemented method"));
        assert_eq!(ttrpc_code(err), None);
    }
}
