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
    env,
    future::Future,
    io::IoSliceMut,
    ops::Deref,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::io::RawFd,
    },
    path::Path,
    sync::Arc,
    time::Duration,
};

use containerd_shim::{
    api::{ExecProcessRequest, Options},
    io_error, other, other_error,
    util::IntoOption,
    Error,
};
use log::{debug, warn};
use nix::{
    cmsg_space,
    sys::{
        socket::{recvmsg, ControlMessageOwned, MsgFlags, UnixAddr},
        termios::tcgetattr,
    },
};
use oci_spec::runtime::{LinuxNamespaceType, Spec};
use runc::{
    io::{Io, NullIo, FIFO},
    options::GlobalOpts,
    Runc, Spawner,
};
use serde::Deserialize;

use super::io::Stdio;

pub const GROUP_LABELS: [&str; 2] = [
    "io.containerd.runc.v2.group",
    "io.kubernetes.cri.sandbox-id",
];
pub const INIT_PID_FILE: &str = "init.pid";
pub const LOG_JSON_FILE: &str = "log.json";
pub const FIFO_SCHEME: &str = "fifo";

const TIMEOUT_DURATION: std::time::Duration = Duration::from_secs(3);

#[derive(Deserialize)]
pub struct Log {
    pub level: String,
    pub msg: String,
}

#[derive(Default)]
pub struct ProcessIO {
    pub uri: Option<String>,
    pub io: Option<Arc<dyn Io>>,
    pub copy: bool,
}

pub fn create_io(
    id: &str,
    _io_uid: u32,
    _io_gid: u32,
    stdio: &Stdio,
) -> containerd_shim::Result<ProcessIO> {
    let mut pio = ProcessIO::default();
    if stdio.is_null() {
        let nio = NullIo::new().map_err(io_error!(e, "new Null Io"))?;
        pio.io = Some(Arc::new(nio));
        return Ok(pio);
    }
    let stdout = stdio.stdout.as_str();
    let scheme_path = stdout.trim().split("://").collect::<Vec<&str>>();
    let scheme: &str;
    if scheme_path.len() <= 1 {
        // no scheme specified, default schema to fifo
        scheme = FIFO_SCHEME;
        pio.uri = Some(format!("{}://{}", scheme, stdout));
    } else {
        scheme = scheme_path[0];
        pio.uri = Some(stdout.to_string());
    }

    if scheme == FIFO_SCHEME {
        debug!(
            "create named pipe io for container {}, stdin: {}, stdout: {}, stderr: {}",
            id,
            stdio.stdin.as_str(),
            stdio.stdout.as_str(),
            stdio.stderr.as_str()
        );
        let io = FIFO {
            stdin: stdio.stdin.to_string().none_if(|x| x.is_empty()),
            stdout: stdio.stdout.to_string().none_if(|x| x.is_empty()),
            stderr: stdio.stderr.to_string().none_if(|x| x.is_empty()),
        };
        pio.io = Some(Arc::new(io));
        pio.copy = false;
    }
    Ok(pio)
}

#[derive(Default, Debug)]
pub struct ShimExecutor {}

pub fn get_spec_from_request(
    req: &ExecProcessRequest,
) -> containerd_shim::Result<oci_spec::runtime::Process> {
    if let Some(val) = req.spec.as_ref() {
        let mut p = serde_json::from_slice::<oci_spec::runtime::Process>(val.value.as_slice())?;
        p.set_terminal(Some(req.terminal));
        Ok(p)
    } else {
        Err(Error::InvalidArgument("no spec in request".to_string()))
    }
}

pub fn check_kill_error(emsg: String) -> Error {
    let emsg = emsg.to_lowercase();
    if emsg.contains("process already finished")
        || emsg.contains("container not running")
        || emsg.contains("no such process")
    {
        Error::NotFoundError("process already finished".to_string())
    } else if emsg.contains("does not exist") {
        Error::NotFoundError("no such container".to_string())
    } else {
        other!("unknown error after kill {}", emsg)
    }
}

const DEFAULT_RUNC_ROOT: &str = "/run/containerd/runc";
const DEFAULT_COMMAND: &str = "runc";

pub fn create_runc(
    runtime: &str,
    namespace: &str,
    bundle: impl AsRef<Path>,
    opts: &Options,
    spawner: Arc<dyn Spawner + Send + Sync>,
) -> containerd_shim::Result<Runc> {
    let runtime = if runtime.is_empty() {
        DEFAULT_COMMAND
    } else {
        runtime
    };
    let root = opts.root.as_str();
    let root = Path::new(if root.is_empty() {
        DEFAULT_RUNC_ROOT
    } else {
        root
    })
    .join(namespace);

    let log = bundle.as_ref().join(LOG_JSON_FILE);
    let mut gopts = GlobalOpts::default()
        .command(runtime)
        .root(root)
        .log(log)
        .log_json()
        .systemd_cgroup(opts.systemd_cgroup);
    gopts.custom_spawner(spawner);
    gopts
        .build()
        .map_err(other_error!("unable to create runc instance"))
}

#[derive(Default)]
pub(crate) struct CreateConfig {}

pub fn receive_socket(stream_fd: RawFd) -> containerd_shim::Result<OwnedFd> {
    let mut buf = [0u8; 4096];
    let mut iovec = [IoSliceMut::new(&mut buf)];
    let mut space = cmsg_space!([RawFd; 2]);
    let (path, fds) =
        match recvmsg::<UnixAddr>(stream_fd, &mut iovec, Some(&mut space), MsgFlags::empty()) {
            Ok(msg) => {
                let iter = msg.cmsgs();
                if let Some(ControlMessageOwned::ScmRights(fds)) = iter?.next() {
                    (iovec[0].deref(), fds)
                } else {
                    return Err(other!("received message is empty"));
                }
            }
            Err(e) => {
                return Err(other!("failed to receive message: {}", e));
            }
        };
    if fds.is_empty() {
        return Err(other!("received message is empty"));
    }
    let path = String::from_utf8(Vec::from(path)).unwrap_or_else(|e| {
        warn!("failed to get path from array {}", e);
        "".to_string()
    });

    let fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };

    let path = path.trim_matches(char::from(0));
    debug!(
        "copy_console: console socket get path: {}, fd: {}",
        path,
        fd.as_raw_fd(),
    );
    tcgetattr(&fd)?;
    Ok(fd)
}

pub fn has_shared_pid_namespace(spec: &Spec) -> bool {
    match spec.linux() {
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
    }
}

/// Returns a temp dir. If the environment variable "XDG_RUNTIME_DIR" is set, return its value.
/// Otherwise if `std::env::temp_dir()` failed, return current dir or return the temp dir depended on OS.
pub(crate) fn xdg_runtime_dir() -> String {
    env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| env::temp_dir().to_str().unwrap_or(".").to_string())
}

pub async fn handle_file_open<F, Fut>(file_op: F) -> Result<tokio::fs::File, tokio::io::Error>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<tokio::fs::File, tokio::io::Error>> + Send,
{
    match tokio::time::timeout(TIMEOUT_DURATION, file_op()).await {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "File operation timed out",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use containerd_shim::{
        api::ExecProcessRequest,
        protos::protobuf::{well_known_types::any::Any, MessageField},
        Error,
    };
    use oci_spec::runtime::{
        LinuxNamespace, LinuxNamespaceBuilder, LinuxNamespaceType, Process, Spec,
    };

    use super::{check_kill_error, create_io, get_spec_from_request, has_shared_pid_namespace};
    use crate::io::Stdio;

    // -----------------------------------------------------------------------
    // check_kill_error: turning runc stderr into a shim error
    // -----------------------------------------------------------------------

    #[test]
    fn kill_error_finished() {
        for msg in [
            "process already finished",
            "container not running",
            "no such process",
            // Casing comes straight off runc stderr, so matching must be insensitive.
            "Container Not Running",
        ] {
            assert!(
                matches!(check_kill_error(msg.to_string()), Error::NotFoundError(_)),
                "{:?} should be reported as not found",
                msg
            );
        }
    }

    #[test]
    fn kill_error_no_container() {
        match check_kill_error("container does not exist".to_string()) {
            Error::NotFoundError(msg) => assert_eq!(msg, "no such container"),
            other => panic!("expected NotFoundError, got {:?}", other),
        }
    }

    #[test]
    fn kill_error_unknown() {
        let err = check_kill_error("disk on fire".to_string());
        assert!(
            !matches!(err, Error::NotFoundError(_)),
            "an unknown failure must not be flattened into not-found"
        );
        assert!(err.to_string().contains("disk on fire"));
    }

    // -----------------------------------------------------------------------
    // has_shared_pid_namespace: whether the shim must reap the container's children
    // -----------------------------------------------------------------------

    fn spec_with_namespaces(namespaces: Option<Vec<LinuxNamespace>>) -> Spec {
        let mut spec = Spec::default();
        let mut linux = spec
            .linux()
            .clone()
            .expect("the default spec has a linux section");
        linux.set_namespaces(namespaces);
        spec.set_linux(Some(linux));
        spec
    }

    fn pid_namespace(path: Option<&str>) -> LinuxNamespace {
        let mut builder = LinuxNamespaceBuilder::default().typ(LinuxNamespaceType::Pid);
        if let Some(path) = path {
            builder = builder.path(PathBuf::from(path));
        }
        builder.build().expect("build pid namespace")
    }

    #[test]
    fn pid_ns() {
        // No path means the container gets a namespace of its own.
        let private = spec_with_namespaces(Some(vec![pid_namespace(None)]));
        assert!(!has_shared_pid_namespace(&private));

        // A path means it joins a namespace that already exists.
        let joined = spec_with_namespaces(Some(vec![pid_namespace(Some("/proc/1/ns/pid"))]));
        assert!(has_shared_pid_namespace(&joined));

        // Nothing isolates the container, so it shares whatever it was given.
        assert!(has_shared_pid_namespace(&spec_with_namespaces(Some(
            vec![]
        ))));
        assert!(has_shared_pid_namespace(&spec_with_namespaces(None)));

        let mut no_linux = Spec::default();
        no_linux.set_linux(None);
        assert!(has_shared_pid_namespace(&no_linux));
    }

    // -----------------------------------------------------------------------
    // get_spec_from_request
    // -----------------------------------------------------------------------

    fn exec_request_with_spec(spec: Option<&Process>, terminal: bool) -> ExecProcessRequest {
        let mut req = ExecProcessRequest {
            terminal,
            ..Default::default()
        };
        if let Some(spec) = spec {
            let mut any = Any::new();
            // Mirrors what containerd sends, even though the shim decodes the
            // payload as JSON without consulting the type url.
            any.type_url = "types.containerd.io/opencontainers/runtime-spec/1/Process".to_string();
            any.value = serde_json::to_vec(spec).expect("encode process spec");
            req.spec = MessageField::some(any);
        }
        req
    }

    #[test]
    fn spec_terminal_override() {
        let mut spec = Process::default();
        spec.set_terminal(Some(false));

        let parsed = get_spec_from_request(&exec_request_with_spec(Some(&spec), true))
            .expect("parse process spec");
        assert_eq!(
            parsed.terminal(),
            Some(true),
            "the exec request decides whether the process gets a tty"
        );

        let parsed = get_spec_from_request(&exec_request_with_spec(Some(&spec), false))
            .expect("parse process spec");
        assert_eq!(parsed.terminal(), Some(false));
    }

    #[test]
    fn spec_missing() {
        let err = get_spec_from_request(&exec_request_with_spec(None, false))
            .expect_err("a spec is required to exec");
        assert!(
            matches!(err, Error::InvalidArgument(_)),
            "expected InvalidArgument, got {:?}",
            err
        );
    }

    // -----------------------------------------------------------------------
    // create_io: choosing the stdio driver from the containerd-supplied paths
    // -----------------------------------------------------------------------

    #[test]
    fn io_null() {
        let stdio = Stdio::default();
        assert!(stdio.is_null());

        let pio = create_io("id", 0, 0, &stdio).expect("create io");
        assert!(pio.io.is_some(), "null stdio still needs a driver");
        assert!(pio.uri.is_none());
        assert!(!pio.copy);
    }

    #[test]
    fn io_fifo() {
        let stdio = Stdio::new("/run/in", "/run/out", "/run/err", false);
        assert!(!stdio.is_null());

        let pio = create_io("id", 0, 0, &stdio).expect("create io");
        assert_eq!(pio.uri.as_deref(), Some("fifo:///run/out"));
        assert!(pio.io.is_some());
        assert!(
            !pio.copy,
            "runc writes to the fifos directly, so the shim does not copy"
        );
    }

    /// Documents current behaviour, which is not obviously the intended one: a
    /// non-fifo scheme yields no io driver and leaves `copy` false, so nothing
    /// is wired up and the container's output goes nowhere. Recorded so a fix
    /// shows up here as a deliberate change.
    #[test]
    fn io_scheme() {
        let stdio = Stdio::new("", "binary:///usr/bin/log", "", false);

        let pio = create_io("id", 0, 0, &stdio).expect("create io");
        assert_eq!(pio.uri.as_deref(), Some("binary:///usr/bin/log"));
        assert!(pio.io.is_none());
        assert!(!pio.copy);
    }
}
