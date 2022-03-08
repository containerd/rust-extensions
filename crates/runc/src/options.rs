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

// Forked from https://github.com/pwFoo/rust-runc/blob/313e6ae5a79b54455b0a242a795c69adf035141a/src/lib.rs

/*
 * Copyright 2020 fsyncd, Berlin, Germany.
 * Additional material, copyright of the containerd authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::error::Error;
use crate::io::Io;
use crate::{utils, DefaultExecutor, Spawner};
use crate::{LogFormat, Runc};

// constants for log format
pub const JSON: &str = "json";
pub const TEXT: &str = "text";

// constants for runc global flags
const DEBUG: &str = "--debug";
const LOG: &str = "--log";
const LOG_FORMAT: &str = "--log-format";
const ROOT: &str = "--root";
const ROOTLESS: &str = "--rootless";
const SYSTEMD_CGROUP: &str = "--systemd-cgroup";

// constants for runc-create/runc-exec flags
const CONSOLE_SOCKET: &str = "--console-socket";
const DETACH: &str = "--detach";
const NO_NEW_KEYRING: &str = "--no-new-keyring";
const NO_PIVOT: &str = "--no-pivot";
const PID_FILE: &str = "--pid-file";

// constants for runc-kill flags
const ALL: &str = "--all";

// constants for runc-delete flags
const FORCE: &str = "--force";

// constant for command
pub const DEFAULT_COMMAND: &str = "runc";

pub trait Args {
    type Output;

    fn args(&self) -> Self::Output;
}

/// Global options builder for the runc binary.
///
/// These options will be passed for all subsequent runc calls.
/// See <https://github.com/opencontainers/runc/blob/main/man/runc.8.md#global-options>
#[derive(Debug, Default)]
pub struct GlobalOpts {
    /// Override the name of the runc binary. If [`None`], `runc` is used.
    command: Option<PathBuf>,
    /// Debug logging.
    ///
    /// If true, debug level logs are emitted.
    debug: bool,
    /// Path to log file.
    log: Option<PathBuf>,
    /// Log format to use.
    log_format: LogFormat,
    /// Path to root directory of container rootfs.
    root: Option<PathBuf>,
    /// Whether to use rootless mode.
    ///
    /// If [`None`], `auto` settings is used.
    /// Note that "auto" is different from explicit "true" or "false".
    rootless: Option<bool>,
    /// Set process group ID (gpid).
    set_pgid: bool,
    /// Use systemd cgroup.
    systemd_cgroup: bool,
    /// Timeout settings for runc command.
    ///
    /// Default is 5 seconds.
    /// This will be used only in AsyncClient.
    timeout: Duration,
    /// executor that runs the commands
    executor: Option<Arc<dyn Spawner + Send + Sync>>,
}

impl GlobalOpts {
    /// Create new config builder with no options.
    pub fn new() -> Self {
        Default::default()
    }

    pub fn command(mut self, command: impl AsRef<Path>) -> Self {
        self.command = Some(command.as_ref().to_path_buf());
        self
    }

    /// Set the root directory to store containers' state.
    ///
    /// The path should be located on tmpfs.
    /// Default is `/run/runc`, or `$XDG_RUNTIME_DIR/runc` for rootless containers.
    pub fn root(mut self, root: impl AsRef<Path>) -> Self {
        self.root = Some(root.as_ref().to_path_buf());
        self
    }

    /// Enable debug logging.
    pub fn debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }

    /// Set the log destination to path.
    ///
    /// The default is to log to stderr.
    pub fn log(mut self, log: impl AsRef<Path>) -> Self {
        self.log = Some(log.as_ref().to_path_buf());
        self
    }

    /// Set the log format (default is text).
    pub fn log_format(mut self, log_format: LogFormat) -> Self {
        self.log_format = log_format;
        self
    }

    /// Set the log format to JSON.
    pub fn log_json(self) -> Self {
        self.log_format(LogFormat::Json)
    }

    /// Set the log format to TEXT.
    pub fn log_text(self) -> Self {
        self.log_format(LogFormat::Text)
    }

    /// Enable systemd cgroup support.
    ///
    /// If this is set, the container spec (`config.json`) is expected to have `cgroupsPath` value in
    // the `slice:prefix:name` form (e.g. `system.slice:runc:434234`).
    pub fn systemd_cgroup(mut self, systemd_cgroup: bool) -> Self {
        self.systemd_cgroup = systemd_cgroup;
        self
    }

    /// Enable or disable rootless mode.
    ///
    // Default is auto, meaning to auto-detect whether rootless should be enabled.
    pub fn rootless(mut self, rootless: bool) -> Self {
        self.rootless = Some(rootless);
        self
    }

    /// Set rootless mode to auto.
    pub fn rootless_auto(mut self) -> Self {
        self.rootless = None;
        self
    }

    pub fn set_pgid(mut self, set_pgid: bool) -> Self {
        self.set_pgid = set_pgid;
        self
    }

    pub fn timeout(&mut self, millis: u64) -> &mut Self {
        self.timeout = Duration::from_millis(millis);
        self
    }

    pub fn custom_spawner(&mut self, executor: Arc<dyn Spawner + Send + Sync>) -> &mut Self {
        self.executor = Some(executor);
        self
    }

    pub fn build(self) -> Result<Runc, Error> {
        self.args()
    }

    fn output(&self) -> Result<(PathBuf, Vec<String>), Error> {
        let path = self
            .command
            .clone()
            .unwrap_or_else(|| PathBuf::from("runc"));

        let command = utils::binary_path(path).ok_or(Error::NotFound)?;

        let mut args = Vec::new();

        // --root path : Set the root directory to store containers' state.
        if let Some(root) = &self.root {
            args.push(ROOT.into());
            args.push(utils::abs_string(root)?);
        }

        // --debug : Enable debug logging.
        if self.debug {
            args.push(DEBUG.into());
        }

        // --log path : Set the log destination to path. The default is to log to stderr.
        if let Some(log_path) = &self.log {
            args.push(LOG.into());
            args.push(utils::abs_string(log_path)?);
        }

        // --log-format text|json : Set the log format (default is text).
        args.push(LOG_FORMAT.into());
        args.push(self.log_format.to_string());

        // --systemd-cgroup : Enable systemd cgroup support.
        if self.systemd_cgroup {
            args.push(SYSTEMD_CGROUP.into());
        }

        // --rootless true|false|auto : Enable or disable rootless mode.
        if let Some(mode) = self.rootless {
            let arg = format!("{}={}", ROOTLESS, mode);
            args.push(arg);
        }
        Ok((command, args))
    }
}

impl Args for GlobalOpts {
    type Output = Result<Runc, Error>;

    fn args(&self) -> Self::Output {
        let (command, args) = self.output()?;
        let executor = if let Some(exec) = self.executor.clone() {
            exec
        } else {
            Arc::new(DefaultExecutor {})
        };
        Ok(Runc {
            command,
            args,
            spawner: executor,
        })
    }
}

#[derive(Clone, Default)]
pub struct CreateOpts {
    pub io: Option<Arc<dyn Io>>,
    /// Path to where a pid file should be created.
    pub pid_file: Option<PathBuf>,
    /// Path to where a console socket should be created.
    pub console_socket: Option<PathBuf>,
    /// Detach from the container's process (only available for run)
    pub detach: bool,
    /// Don't use pivot_root to jail process inside rootfs.
    pub no_pivot: bool,
    /// A new session keyring for the container will not be created.
    pub no_new_keyring: bool,
}

impl Args for CreateOpts {
    type Output = Result<Vec<String>, Error>;

    fn args(&self) -> Self::Output {
        let mut args: Vec<String> = vec![];
        if let Some(pid_file) = &self.pid_file {
            args.push(PID_FILE.to_string());
            args.push(utils::abs_string(pid_file)?);
        }
        if let Some(console_socket) = &self.console_socket {
            args.push(CONSOLE_SOCKET.to_string());
            args.push(utils::abs_string(console_socket)?);
        }
        if self.no_pivot {
            args.push(NO_PIVOT.to_string());
        }
        if self.no_new_keyring {
            args.push(NO_NEW_KEYRING.to_string());
        }
        if self.detach {
            args.push(DETACH.to_string());
        }
        Ok(args)
    }
}

impl CreateOpts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn io(mut self, io: Arc<dyn Io>) -> Self {
        self.io = Some(io);
        self
    }

    pub fn pid_file<P>(mut self, pid_file: P) -> Self
    where
        P: AsRef<Path>,
    {
        self.pid_file = Some(pid_file.as_ref().to_path_buf());
        self
    }

    pub fn console_socket<P>(mut self, console_socket: P) -> Self
    where
        P: AsRef<Path>,
    {
        self.console_socket = Some(console_socket.as_ref().to_path_buf());
        self
    }

    pub fn detach(mut self, detach: bool) -> Self {
        self.detach = detach;
        self
    }

    pub fn no_pivot(mut self, no_pivot: bool) -> Self {
        self.no_pivot = no_pivot;
        self
    }

    pub fn no_new_keyring(mut self, no_new_keyring: bool) -> Self {
        self.no_new_keyring = no_new_keyring;
        self
    }
}

/// Container execution options
#[derive(Clone, Default)]
pub struct ExecOpts {
    pub io: Option<Arc<dyn Io>>,
    /// Path to where a pid file should be created.
    pub pid_file: Option<PathBuf>,
    /// Path to where a console socket should be created.
    pub console_socket: Option<PathBuf>,
    /// Detach from the container's process (only available for run)
    pub detach: bool,
}

impl Args for ExecOpts {
    type Output = Result<Vec<String>, Error>;

    fn args(&self) -> Self::Output {
        let mut args: Vec<String> = vec![];
        if let Some(pid_file) = &self.pid_file {
            args.push(PID_FILE.to_string());
            args.push(utils::abs_string(pid_file)?);
        }
        if let Some(console_socket) = &self.console_socket {
            args.push(CONSOLE_SOCKET.to_string());
            args.push(utils::abs_string(console_socket)?);
        }
        if self.detach {
            args.push(DETACH.to_string());
        }
        Ok(args)
    }
}

impl ExecOpts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn io(mut self, io: Arc<dyn Io>) -> Self {
        self.io = Some(io);
        self
    }

    pub fn pid_file<P>(mut self, pid_file: P) -> Self
    where
        P: AsRef<Path>,
    {
        self.pid_file = Some(pid_file.as_ref().to_path_buf());
        self
    }

    pub fn console_socket<P>(mut self, console_socket: P) -> Self
    where
        P: AsRef<Path>,
    {
        self.console_socket = Some(console_socket.as_ref().to_path_buf());
        self
    }

    pub fn detach(mut self, detach: bool) -> Self {
        self.detach = detach;
        self
    }
}

/// Container deletion options
#[derive(Debug, Clone, Default)]
pub struct DeleteOpts {
    /// Forcibly delete the container if it is still running
    pub force: bool,
}

impl Args for DeleteOpts {
    type Output = Vec<String>;

    fn args(&self) -> Self::Output {
        let mut args: Vec<String> = vec![];
        if self.force {
            args.push(FORCE.to_string());
        }
        args
    }
}

impl DeleteOpts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }
}

/// Container killing options
#[derive(Debug, Clone, Default)]
pub struct KillOpts {
    /// Seng the kill signal to all the processes inside the container
    pub all: bool,
}

impl Args for KillOpts {
    type Output = Vec<String>;

    fn args(&self) -> Self::Output {
        let mut args: Vec<String> = vec![];
        if self.all {
            args.push(ALL.to_string());
        }
        args
    }
}

impl KillOpts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn all(mut self, all: bool) -> Self {
        self.all = all;
        self
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::*;

    const ARGS_FAIL_MSG: &str = "Args.args() failed.";

    #[test]
    fn create_opts_test() {
        assert_eq!(
            CreateOpts::new().args().expect(ARGS_FAIL_MSG),
            vec![String::new(); 0]
        );

        assert_eq!(
            CreateOpts::new().pid_file(".").args().expect(ARGS_FAIL_MSG),
            vec![
                "--pid-file".to_string(),
                env::current_dir()
                    .unwrap()
                    .to_string_lossy()
                    .parse::<String>()
                    .unwrap()
            ]
        );

        assert_eq!(
            CreateOpts::new()
                .console_socket("..")
                .args()
                .expect(ARGS_FAIL_MSG),
            vec![
                "--console-socket".to_string(),
                env::current_dir()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .to_string_lossy()
                    .parse::<String>()
                    .unwrap()
            ]
        );

        assert_eq!(
            CreateOpts::new()
                .detach(true)
                .no_pivot(true)
                .no_new_keyring(true)
                .args()
                .expect(ARGS_FAIL_MSG),
            vec![
                "--no-pivot".to_string(),
                "--no-new-keyring".to_string(),
                "--detach".to_string(),
            ]
        );
    }

    #[test]
    fn exec_opts_test() {
        assert_eq!(
            ExecOpts::new().args().expect(ARGS_FAIL_MSG),
            vec![String::new(); 0]
        );

        assert_eq!(
            ExecOpts::new().pid_file(".").args().expect(ARGS_FAIL_MSG),
            vec![
                "--pid-file".to_string(),
                env::current_dir()
                    .unwrap()
                    .to_string_lossy()
                    .parse::<String>()
                    .unwrap()
            ]
        );

        assert_eq!(
            ExecOpts::new()
                .console_socket("..")
                .args()
                .expect(ARGS_FAIL_MSG),
            vec![
                "--console-socket".to_string(),
                env::current_dir()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .to_string_lossy()
                    .parse::<String>()
                    .unwrap()
            ]
        );

        assert_eq!(
            ExecOpts::new().detach(true).args().expect(ARGS_FAIL_MSG),
            vec!["--detach".to_string(),]
        );
    }

    #[test]
    fn delete_opts_test() {
        assert_eq!(
            DeleteOpts::new().force(false).args(),
            vec![String::new(); 0]
        );

        assert_eq!(
            DeleteOpts::new().force(true).args(),
            vec!["--force".to_string()],
        );
    }

    #[test]
    fn kill_opts_test() {
        assert_eq!(KillOpts::new().all(false).args(), vec![String::new(); 0]);

        assert_eq!(KillOpts::new().all(true).args(), vec!["--all".to_string()],);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn global_opts_test() {
        let cfg = GlobalOpts::default().command("true");
        let runc = cfg.build().unwrap();
        let args = &runc.args;
        assert_eq!(args.len(), 2);
        assert!(args.contains(&LOG_FORMAT.to_string()));
        assert!(args.contains(&TEXT.to_string()));

        let cfg = GlobalOpts::default().command("/bin/true");
        let runc = cfg.build().unwrap();
        assert_eq!(runc.args.len(), 2);

        let cfg = GlobalOpts::default()
            .command("true")
            .root("/tmp")
            .debug(true)
            .log("/tmp/runc.log")
            .log_json()
            .systemd_cgroup(true)
            .rootless(true);
        let runc = cfg.build().unwrap();
        let args = &runc.args;
        assert!(args.contains(&ROOT.to_string()));
        assert!(args.contains(&DEBUG.to_string()));
        assert!(args.contains(&"/tmp".to_string()));
        assert!(args.contains(&LOG.to_string()));
        assert!(args.contains(&"/tmp/runc.log".to_string()));
        assert!(args.contains(&LOG_FORMAT.to_string()));
        assert!(args.contains(&JSON.to_string()));
        assert!(args.contains(&"--rootless=true".to_string()));
        assert!(args.contains(&SYSTEMD_CGROUP.to_string()));
        assert_eq!(args.len(), 9);
    }
}
