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

#![cfg_attr(feature = "docs", doc = include_str!("../README.md"))]

//! A crate for consuming the runc binary in your Rust applications, similar to
//! [go-runc](https://github.com/containerd/go-runc) for Go.
use std::{
    fmt::{self, Debug, Display},
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::Arc,
};

#[cfg(feature = "async")]
pub use crate::asynchronous::*;
#[cfg(not(feature = "async"))]
pub use crate::synchronous::*;

#[cfg(feature = "async")]
pub mod asynchronous;
pub mod container;
pub mod error;
pub mod events;
#[cfg(not(feature = "async"))]
pub mod synchronous;

#[cfg(feature = "async")]
pub mod monitor;
pub mod options;
pub mod utils;

const JSON: &str = "json";
const TEXT: &str = "text";

const DEBUG: &str = "--debug";
const LOG: &str = "--log";
const LOG_FORMAT: &str = "--log-format";
const ROOT: &str = "--root";
const ROOTLESS: &str = "--rootless";
const SYSTEMD_CGROUP: &str = "--systemd-cgroup";

pub type Result<T> = std::result::Result<T, crate::error::Error>;

/// Response is for (pid, exit status, outputs).
#[derive(Debug, Clone)]
pub struct Response {
    pub pid: u32,
    pub status: ExitStatus,
    pub output: String,
}

#[derive(Debug, Clone)]
pub struct Version {
    pub runc_version: Option<String>,
    pub spec_version: Option<String>,
    pub commit: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub enum LogFormat {
    Json,
    #[default]
    Text,
}

impl Display for LogFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogFormat::Json => write!(f, "{}", JSON),
            LogFormat::Text => write!(f, "{}", TEXT),
        }
    }
}

#[cfg(not(feature = "async"))]
pub type Command = std::process::Command;

#[cfg(feature = "async")]
pub type Command = tokio::process::Command;

#[derive(Debug, Clone)]
pub struct Runc {
    command: PathBuf,
    global_args: RuncGlobalArgs,
    spawner: Arc<dyn Spawner + Send + Sync>,
}

#[derive(Debug, Clone, Default)]
pub struct RuncGlobalArgs {
    pub debug: Option<bool>,
    pub log: Option<PathBuf>,
    pub log_format: Option<String>,
    pub root: Option<PathBuf>,
    pub systemd_cgroup: Option<bool>,
    pub rootless: Option<bool>,
}

impl Runc {
    fn command(&self, args: &[String]) -> Result<Command> {
        let custom_global_args = RuncGlobalArgs {
            debug: None,
            log: None,
            log_format: None,
            root: None,
            systemd_cgroup: None,
            rootless: None,
        };
        self.command_with_global_args(args, custom_global_args)
    }

    fn command_with_global_args(
        &self,
        args: &[String],
        custom_global_args: RuncGlobalArgs,
    ) -> Result<Command> {
        let mut global_args_vec: Vec<String> = Vec::new();
        if let Some(custom_debug) = custom_global_args.debug {
            global_args_vec.push(DEBUG.to_string());
            global_args_vec.push(custom_debug.to_string());
        } else if let Some(debug) = self.global_args.debug {
            global_args_vec.push(DEBUG.to_string());
            global_args_vec.push(debug.to_string());
        }

        if let Some(custom_log) = custom_global_args.log {
            global_args_vec.push(LOG.to_string());
            global_args_vec.push(custom_log.to_string_lossy().to_string());
        } else if let Some(log) = &self.global_args.log {
            global_args_vec.push(LOG.to_string());
            global_args_vec.push(log.to_string_lossy().to_string());
        }

        if let Some(custom_log_format) = custom_global_args.log_format {
            global_args_vec.push(LOG_FORMAT.to_string());
            global_args_vec.push(custom_log_format);
        } else if let Some(log_format) = &self.global_args.log_format {
            global_args_vec.push(LOG_FORMAT.to_string());
            global_args_vec.push(log_format.to_string());
        }

        if let Some(custom_root) = custom_global_args.root {
            global_args_vec.push(ROOT.to_string());
            global_args_vec.push(custom_root.to_string_lossy().to_string());
        } else if let Some(root) = &self.global_args.root {
            global_args_vec.push(ROOT.to_string());
            global_args_vec.push(root.to_string_lossy().to_string());
        }

        if let Some(systemd_cgroup) = custom_global_args.systemd_cgroup {
            global_args_vec.push(SYSTEMD_CGROUP.to_string());
            global_args_vec.push(systemd_cgroup.to_string());
        } else if let Some(systemd_cgroup) = self.global_args.systemd_cgroup {
            global_args_vec.push(SYSTEMD_CGROUP.to_string());
            global_args_vec.push(systemd_cgroup.to_string());
        }

        if let Some(custom_rootless) = custom_global_args.rootless {
            global_args_vec.push(ROOTLESS.to_string());
            global_args_vec.push(custom_rootless.to_string());
        } else if let Some(rootless) = self.global_args.rootless {
            global_args_vec.push(ROOTLESS.to_string());
            global_args_vec.push(rootless.to_string());
        }

        let args = [global_args_vec, args.to_vec()].concat();
        let mut cmd = Command::new(&self.command);

        // Default to piped stdio, and they may be override by command options.
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // NOTIFY_SOCKET introduces a special behavior in runc but should only be set if invoked from systemd
        cmd.args(&args).env_remove("NOTIFY_SOCKET");

        Ok(cmd)
    }
}
