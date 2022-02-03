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
use std::time::Duration;

use crate::error::Error;
use crate::options::Args;
use crate::utils::{self, DEBUG, DEFAULT_COMMAND, LOG, LOG_FORMAT, ROOT, ROOTLESS, SYSTEMD_CGROUP};
use crate::LogFormat;

/// Inner struct for runc configuration
#[derive(Debug, Clone, Default)]
pub struct RuncConfig {
    /// This field is set to overrides the name of the runc binary. If [`None`], "runc" is used.
    command: Option<PathBuf>,
    /// Path to root directory of container rootfs.
    root: Option<PathBuf>,
    /// Debug logging. If true, debug level logs are emitted.
    debug: bool,
    /// Path to log file.
    log: Option<PathBuf>,
    /// Specifyng log format. Here, json or text is available. Default is "text" and interpreted as text if [`None`].
    log_format: Option<LogFormat>,
    // FIXME: implementation of pdeath_signal is suspended due to difficulties, maybe it's favorable to use signal-hook crate.
    // pdeath_signal: XXX,
    /// Using systemd cgroup.
    systemd_cgroup: bool,
    /// Setting process group ID(gpid).
    set_pgid: bool,
    // FIXME: implementation of extra_args is suspended due to difficulties.
    // criu: String,
    /// Setting of whether using rootless mode or not. If [`None`], "auto" settings is used. Note that "auto" is different from explicit "true" or "false".
    rootless: Option<bool>,
    // FIXME: implementation of extra_args is suspended due to difficulties.
    // extra_args: Vec<String>,
    /// Timeout settings for runc command. Default is 5 seconds.
    /// This will be used only in AsyncClient.
    timeout: Option<Duration>,
}

impl RuncConfig {
    pub fn command<P>(&mut self, command: P)
    where
        P: AsRef<Path>,
    {
        self.command = Some(command.as_ref().to_path_buf());
    }

    pub fn root<P>(&mut self, root: P)
    where
        P: AsRef<Path>,
    {
        self.root = Some(root.as_ref().to_path_buf());
    }

    pub fn debug(&mut self, debug: bool) {
        self.debug = debug;
    }

    pub fn log<P>(&mut self, log: P)
    where
        P: AsRef<Path>,
    {
        self.log = Some(log.as_ref().to_path_buf());
    }

    pub fn log_format(&mut self, log_format: LogFormat) {
        self.log_format = Some(log_format);
    }

    pub fn log_format_json(&mut self) {
        self.log_format = Some(LogFormat::Json);
    }

    pub fn log_format_text(&mut self) {
        self.log_format = Some(LogFormat::Text);
    }

    pub fn systemd_cgroup(&mut self, systemd_cgroup: bool) {
        self.systemd_cgroup = systemd_cgroup;
    }

    // FIXME: criu is not supported now
    // pub fn criu(&mut self, criu: bool) {
    //     self.criu = criu;
    // }

    pub fn rootless(&mut self, rootless: bool) {
        self.rootless = Some(rootless);
    }

    pub fn set_pgid(&mut self, set_pgid: bool) {
        self.set_pgid = set_pgid;
    }

    pub fn rootless_auto(&mut self) {
        let _ = self.rootless.take();
    }

    pub fn timeout(&mut self, millis: u64) {
        self.timeout = Some(Duration::from_millis(millis));
    }

    pub fn build(&mut self) -> Result<Runc, Error> {
        let command = utils::binary_path(
            self.command
                .clone()
                .unwrap_or_else(|| PathBuf::from(DEFAULT_COMMAND)),
        )
        .ok_or(Error::NotFound)?;
        Ok(Runc {
            command,
            root: self.root.clone(),
            debug: self.debug,
            log: self.log.clone(),
            log_format: self.log_format.clone().unwrap_or(LogFormat::Text),
            // self.pdeath_signal: self.pdeath_signal,
            systemd_cgroup: self.systemd_cgroup,
            set_pgid: self.set_pgid,
            // criu: self.criu,
            rootless: self.rootless,
            // extra_args: self.extra_args,
            timeout: self.timeout.unwrap_or_else(|| Duration::from_millis(5000)),
        })
    }
}

/// Inner Runtime for RuncClient/RuncAsyncClient
#[derive(Debug, Clone)]
pub struct Runc {
    pub command: PathBuf,
    pub root: Option<PathBuf>,
    pub debug: bool,
    pub log: Option<PathBuf>,
    pub log_format: LogFormat,
    // pdeath_signal: XXX,
    pub set_pgid: bool,
    // criu: bool,
    pub systemd_cgroup: bool,
    pub rootless: Option<bool>,
    // extra_args: Vec<String>,
    pub timeout: Duration,
}

impl Args for Runc {
    type Output = Result<Vec<String>, Error>;
    fn args(&self) -> Self::Output {
        let mut args: Vec<String> = vec![];
        if let Some(root) = &self.root {
            args.push(ROOT.to_string());
            args.push(utils::abs_string(root)?);
        }
        if self.debug {
            args.push(DEBUG.to_string());
        }
        if let Some(log_path) = &self.log {
            args.push(LOG.to_string());
            args.push(utils::abs_string(log_path)?);
        }
        args.push(LOG_FORMAT.to_string());
        args.push(self.log_format.to_string());
        // if self.criu {
        //     args.push(CRIU.to_string());
        // }
        if self.systemd_cgroup {
            args.push(SYSTEMD_CGROUP.to_string());
        }
        if let Some(rootless) = self.rootless {
            let arg = format!("{}={}", ROOTLESS, rootless);
            args.push(arg);
        }
        // if self.extra_args.len() > 0 {
        //     args.append(&mut self.extra_args.clone())
        // }
        Ok(args)
    }
}
