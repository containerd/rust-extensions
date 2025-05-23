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
    args: Vec<String>,
    spawner: Arc<dyn Spawner + Send + Sync>,
}

impl Runc {
    fn command(&self, args: &[String]) -> Result<Command> {
        let args = [&self.args, args].concat();
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
