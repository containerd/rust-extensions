/*
   copyright the containerd authors.

   licensed under the apache license, version 2.0 (the "license");
   you may not use this file except in compliance with the license.
   you may obtain a copy of the license at

       http://www.apache.org/licenses/license-2.0

   unless required by applicable law or agreed to in writing, software
   distributed under the license is distributed on an "as is" basis,
   without warranties or conditions of any kind, either express or implied.
   see the license for the specific language governing permissions and
   limitations under the license.
*/

// Forked from https://github.com/pwFoo/rust-runc/blob/master/src/lib.rs
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

//! A crate for consuming the runc binary in your Rust applications, similar to [go-runc](https://github.com/containerd/go-runc) for Go.

use std::fmt::{self, Display};
use std::io::Write;
use std::path::Path;
use std::process::{ExitStatus, Output, Stdio};
use std::time::Duration;

use tempfile::NamedTempFile;

// suspended for difficulties
// pub mod console;
pub mod container;
pub mod error;
pub mod events;
pub mod io;
pub mod monitor;
pub mod options;
mod runc;
pub mod specs;
mod utils;

use crate::container::Container;
use crate::error::Error;
use crate::events::{Event, Stats};
use crate::monitor::{DefaultMonitor, Exit, ProcessMonitor};
use crate::options::*;
use crate::specs::{LinuxResources, Process};
use crate::utils::{JSON, TEXT};

type Result<T> = std::result::Result<T, crate::error::Error>;

/// RuncResponse is for (pid, exit status, outputs).
#[derive(Debug, Clone)]
pub struct RuncResponse {
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

#[derive(Debug, Clone)]
pub enum LogFormat {
    Json,
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

/// Configuration for runc client.
///
/// This struct provide chaining interface like, for example, [`std::fs::OpenOptions`].
/// Note that you cannot access the members of RuncConfig directly.
///
/// # Example
///
/// ```no_run
/// use containerd_runc_rust as runc;
///
/// let config = runc::RuncConfig::new()
///     .root("./new_root")
///     .debug(false)
///     .log("/path/to/logfile.json")
///     .log_format(runc::LogFormat::Json)
///     .rootless(true);
/// let client = config.build();
/// ```
#[derive(Debug, Clone, Default)]
pub struct RuncConfig(runc::RuncConfig);

impl RuncConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn command<P>(&mut self, command: P) -> &mut Self
    where
        P: AsRef<Path>,
    {
        self.0.command(command);
        self
    }

    pub fn root<P>(&mut self, root: P) -> &mut Self
    where
        P: AsRef<Path>,
    {
        self.0.root(root);
        self
    }

    pub fn debug(&mut self, debug: bool) -> &mut Self {
        self.0.debug(debug);
        self
    }

    pub fn log<P>(&mut self, log: P) -> &mut Self
    where
        P: AsRef<Path>,
    {
        self.0.log(log);
        self
    }

    pub fn log_format(&mut self, log_format: LogFormat) -> &mut Self {
        self.0.log_format(log_format);
        self
    }

    pub fn log_format_json(&mut self) -> &mut Self {
        self.0.log_format_json();
        self
    }

    pub fn log_format_text(&mut self) -> &mut Self {
        self.0.log_format_text();
        self
    }

    pub fn systemd_cgroup(&mut self, systemd_cgroup: bool) -> &mut Self {
        self.0.systemd_cgroup(systemd_cgroup);
        self
    }

    // FIXME: criu is not supported now
    // pub fn criu(mut self, criu: bool) -> Self {
    //     self.0.criu(criu);
    //     self
    // }

    pub fn rootless(&mut self, rootless: bool) -> &mut Self {
        self.0.rootless(rootless);
        self
    }

    pub fn set_pgid(&mut self, set_pgid: bool) -> &mut Self {
        self.0.set_pgid(set_pgid);
        self
    }

    pub fn rootless_auto(&mut self) -> &mut Self {
        self.0.rootless_auto();
        self
    }

    pub fn timeout(&mut self, millis: u64) -> &mut Self {
        self.0.timeout(millis);
        self
    }

    pub fn build(&mut self) -> Result<RuncClient> {
        Ok(RuncClient(self.0.build()?))
    }

    pub fn build_async(&mut self) -> Result<RuncAsyncClient> {
        Ok(RuncAsyncClient(self.0.build()?))
    }
}

#[derive(Debug, Clone)]
pub struct RuncClient(runc::Runc);

impl RuncClient {
    /// Create a new runc client from the supplied configuration
    pub fn from_config(mut config: RuncConfig) -> Result<Self> {
        config.build()
    }

    #[cfg(target_os = "linux")]
    pub fn command(&self, args: &[String]) -> Result<std::process::Command> {
        let args = [&self.0.args()?, args].concat();
        let mut cmd = std::process::Command::new(&self.0.command);
        cmd.args(&args).env_remove("NOTIFY_SOCKET"); // NOTIFY_SOCKET introduces a special behavior in runc but should only be set if invoked from systemd
        Ok(cmd)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn command(&self, args: &[String]) -> Result<()> {
        Err(Error::Unimplemented("command".to_string()))
    }

    pub fn checkpoint(&self) -> Result<()> {
        Err(Error::Unimplemented("checkpoint".to_string()))
    }

    fn launch(
        &self,
        mut cmd: std::process::Command,
        combined_output: bool,
        forget: bool,
    ) -> Result<RuncResponse> {
        let child = cmd.spawn().map_err(Error::ProcessSpawnFailed)?;
        let pid = child.id();
        let result = child.wait_with_output().map_err(Error::InvalidCommand)?;
        let status = result.status;
        let stdout = String::from_utf8(result.stdout).unwrap();
        let stderr = String::from_utf8(result.stderr).unwrap();
        if forget {
            // reserve fds of pipes for after use
            std::mem::forget(cmd);
        }
        if status.success() {
            if combined_output {
                Ok(RuncResponse {
                    pid,
                    status,
                    output: stdout + stderr.as_str(),
                })
            } else {
                Ok(RuncResponse {
                    pid,
                    status,
                    output: stdout,
                })
            }
        } else {
            // [DEBUG]
            // let stdout = stdout + &args.join(" ");
            Err(Error::CommandFailed {
                status,
                stdout,
                stderr,
            })
        }
    }

    /// Create a new container
    pub fn create<P>(&self, id: &str, bundle: P, opts: Option<&CreateOpts>) -> Result<RuncResponse>
    where
        P: AsRef<Path>,
    {
        let mut args = vec![
            "create".to_string(),
            "--bundle".to_string(),
            utils::abs_string(bundle)?,
        ];
        if let Some(opts) = opts {
            args.append(&mut opts.args()?);
        }
        args.push(id.to_string());
        let mut cmd = self.command(&args)?;
        match opts {
            Some(CreateOpts { io: Some(_io), .. }) => {
                _io.set(&mut cmd).map_err(Error::UnavailableIO)?;
                let res = self.launch(cmd, true, true)?;
                _io.close_after_start();
                Ok(res)
            }
            _ => self.launch(cmd, true, false),
        }
    }

    /// Delete a container
    pub fn delete(&self, id: &str, opts: Option<&DeleteOpts>) -> Result<()> {
        let mut args = vec!["delete".to_string()];
        if let Some(opts) = opts {
            args.append(&mut opts.args());
        }
        args.push(id.to_string());
        self.launch(self.command(&args)?, true, false)?;
        Ok(())
    }

    /// Execute an additional process inside the container
    pub fn exec(&self, id: &str, spec: &Process, opts: Option<&ExecOpts>) -> Result<()> {
        let (mut temp_file, file_name): (NamedTempFile, String) =
            utils::make_temp_file_in_runtime_dir()?;
        {
            let f = temp_file.as_file_mut();
            let spec_json =
                serde_json::to_string(spec).map_err(Error::JsonDeserializationFailed)?;
            f.write(spec_json.as_bytes())
                .map_err(Error::SpecFileCreationError)?;
            f.flush().map_err(Error::SpecFileCreationError)?;
        }
        let mut args = vec!["exec".to_string(), "process".to_string(), file_name];
        if let Some(opts) = opts {
            args.append(&mut opts.args()?);
        }
        args.push(id.to_string());
        let mut cmd = self.command(&args)?;
        let forget = match opts {
            Some(ExecOpts { io: Some(_io), .. }) => {
                _io.set(&mut cmd).map_err(Error::UnavailableIO)?;
                true
            }
            _ => false,
        };
        let _ = self.launch(cmd, true, forget)?;
        Ok(())
    }

    /// Send the specified signal to processes inside the container
    pub fn kill(&self, id: &str, sig: u32, opts: Option<&KillOpts>) -> Result<()> {
        let mut args = vec!["kill".to_string()];
        if let Some(opts) = opts {
            args.append(&mut opts.args());
        }
        args.push(id.to_string());
        args.push(sig.to_string());
        let _ = self.launch(self.command(&args)?, true, false)?;
        Ok(())
    }

    /// List all containers associated with this runc instance
    pub fn list(&self) -> Result<Vec<Container>> {
        let args = ["list".to_string(), "--format-json".to_string()];
        let res = self.launch(self.command(&args)?, true, false)?;
        let output = res.output.trim();
        // Ugly hack to work around golang
        Ok(if output == "null" {
            Vec::new()
        } else {
            serde_json::from_str(output).map_err(Error::JsonDeserializationFailed)?
        })
    }

    /// Pause a container
    pub fn pause(&self, id: &str) -> Result<()> {
        let args = ["pause".to_string(), id.to_string()];
        let _ = self.launch(self.command(&args)?, true, false)?;
        Ok(())
    }

    pub fn restore(&self) -> Result<()> {
        Err(Error::Unimplemented("restore".to_string()))
    }

    /// Resume a container
    pub fn resume(&self, id: &str) -> Result<()> {
        let args = ["pause".to_string(), id.to_string()];
        let _ = self.launch(self.command(&args)?, true, false)?;
        Ok(())
    }

    /// Run the create, start, delete lifecycle of the container and return its exit status
    pub fn run<P>(&self, id: &str, bundle: P, opts: Option<&CreateOpts>) -> Result<RuncResponse>
    where
        P: AsRef<Path>,
    {
        let mut args = vec!["run".to_string(), "--bundle".to_string()];
        if let Some(opts) = opts {
            args.append(&mut opts.args()?);
        }
        args.push(utils::abs_string(bundle)?);
        args.push(id.to_string());
        let mut cmd = self.command(&args)?;
        let forget = match opts {
            Some(CreateOpts { io: Some(_io), .. }) => {
                _io.set(&mut cmd).map_err(Error::UnavailableIO)?;
                true
            }
            _ => false,
        };

        self.launch(self.command(&args)?, true, forget)
    }

    /// Start an already created container
    pub fn start(&self, id: &str) -> Result<RuncResponse> {
        let args = ["start".to_string(), id.to_string()];
        self.launch(self.command(&args)?, true, false)
    }

    /// Return the state of a container
    pub fn state(&self, id: &str) -> Result<Container> {
        let args = ["state".to_string(), id.to_string()];
        let res = self.launch(self.command(&args)?, true, false)?;
        Ok(serde_json::from_str(&res.output).map_err(Error::JsonDeserializationFailed)?)
    }

    /// Update a container with the provided resource spec
    pub fn update(&self, id: &str, resources: &LinuxResources) -> Result<()> {
        let (mut temp_file, file_name): (NamedTempFile, String) =
            utils::make_temp_file_in_runtime_dir()?;
        {
            let f = temp_file.as_file_mut();
            let spec_json =
                serde_json::to_string(resources).map_err(Error::JsonDeserializationFailed)?;
            f.write(spec_json.as_bytes())
                .map_err(Error::SpecFileCreationError)?;
            f.flush().map_err(Error::SpecFileCreationError)?;
        }
        let args = [
            "update".to_string(),
            "--resources".to_string(),
            file_name,
            id.to_string(),
        ];
        self.launch(self.command(&args)?, true, false)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct RuncAsyncClient(runc::Runc);

// As monitor instance never have to be mutable (it has only &self methods), declare it as const.
const MONITOR: DefaultMonitor = DefaultMonitor::new();

/// Async client for runc
/// Note that you MUST use this client on tokio runtime, as this client internally use [`tokio::process::Command`]
/// and some other utilities.
impl RuncAsyncClient {
    /// Create a new runc client from the supplied configuration
    pub fn from_config(mut config: RuncConfig) -> Result<Self> {
        config.build_async()
    }

    pub fn command(&self, args: &[String]) -> Result<tokio::process::Command> {
        let args = [&self.0.args()?, args].concat();
        let mut cmd = tokio::process::Command::new(&self.0.command);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        cmd.args(&args).env_remove("NOTIFY_SOCKET"); // NOTIFY_SOCKET introduces a special behavior in runc but should only be set if invoked from systemd
        Ok(cmd)
    }

    pub async fn launch(
        &self,
        cmd: tokio::process::Command,
        combined_output: bool,
        forget: bool,
    ) -> Result<RuncResponse> {
        let (tx, rx) = tokio::sync::oneshot::channel::<Exit>();
        let start = MONITOR.start(cmd, tx, forget);
        let wait = MONITOR.wait(rx);
        let (
            Output {
                status,
                stdout,
                stderr,
            },
            Exit { pid, .. },
        ) = tokio::try_join!(start, wait).map_err(Error::InvalidCommand)?;

        // ugly hack to work around
        let stdout = String::from_utf8(stdout)
            .expect("returned non-utf8 characters from container process.");
        let stderr = String::from_utf8(stderr)
            .expect("returned non-utf8 characters from container process.");

        if status.success() {
            if combined_output {
                Ok(RuncResponse {
                    pid,
                    status,
                    output: stdout + stderr.as_str(),
                })
            } else {
                Ok(RuncResponse {
                    pid,
                    status,
                    output: stdout,
                })
            }
        } else {
            Err(Error::CommandFailed {
                status,
                stdout,
                stderr,
            })
        }
    }

    pub async fn checkpoint(&self) -> Result<()> {
        Err(Error::Unimplemented("checkpoint".to_string()))
    }

    /// Create a new container
    pub async fn create<P>(&self, id: &str, bundle: P, opts: Option<&CreateOpts>) -> Result<()>
    where
        P: AsRef<Path>,
    {
        let mut args = vec![
            "create".to_string(),
            "--bundle".to_string(),
            utils::abs_string(bundle)?,
        ];
        if let Some(opts) = opts {
            args.append(&mut opts.args()?);
        }
        args.push(id.to_string());
        let mut cmd = self.command(&args)?;
        match opts {
            Some(CreateOpts { io: Some(_io), .. }) => {
                _io.set_tk(&mut cmd).map_err(Error::UnavailableIO)?;
                let (tx, rx) = tokio::sync::oneshot::channel::<Exit>();
                let start = MONITOR.start(cmd, tx, true);
                let wait = MONITOR.wait(rx);
                _io.close_after_start();
                let (
                    Output {
                        status,
                        stdout,
                        stderr,
                    },
                    _,
                ) = tokio::try_join!(start, wait).map_err(Error::InvalidCommand)?;

                let stdout = String::from_utf8(stdout).unwrap();
                let stderr = String::from_utf8(stderr).unwrap();
                if !status.success() {
                    return Err(Error::CommandFailed {
                        status,
                        stdout,
                        stderr,
                    });
                }
            }
            _ => {
                let _ = self.launch(cmd, true, false).await?;
            }
        }
        Ok(())
    }

    /// Delete a container
    pub async fn delete(&self, id: &str, opts: Option<&DeleteOpts>) -> Result<()> {
        let mut args = vec!["delete".to_string()];
        if let Some(opts) = opts {
            args.append(&mut opts.args());
        }
        args.push(id.to_string());
        let _ = self.launch(self.command(&args)?, true, false).await?;
        Ok(())
    }

    /// Return an event stream of container notifications
    pub async fn events(&self, _id: &str, _interval: &Duration) -> Result<()> {
        Err(Error::Unimplemented("events".to_string()))
    }

    /// Execute an additional process inside the container
    pub async fn exec(&self, _id: &str, _spec: &Process, _opts: Option<&ExecOpts>) -> Result<()> {
        Err(Error::Unimplemented("exec".to_string()))
    }

    /// Send the specified signal to processes inside the container
    pub async fn kill(&self, id: &str, sig: u32, opts: Option<&KillOpts>) -> Result<()> {
        let mut args = vec!["kill".to_string()];
        if let Some(opts) = opts {
            args.append(&mut opts.args());
        }
        args.push(id.to_string());
        args.push(sig.to_string());
        let _ = self.launch(self.command(&args)?, true, false).await?;
        Ok(())
    }

    /// List all containers associated with this runc instance
    pub async fn list(&self) -> Result<Vec<Container>> {
        let args = ["list".to_string(), "--format-json".to_string()];
        let res = self.launch(self.command(&args)?, true, false).await?;
        let output = res.output.trim();
        // Ugly hack to work around golang
        Ok(if output == "null" {
            Vec::new()
        } else {
            serde_json::from_str(output).map_err(Error::JsonDeserializationFailed)?
        })
    }

    /// Pause a container
    pub async fn pause(&self, id: &str) -> Result<()> {
        let args = ["pause".to_string(), id.to_string()];
        let _ = self.launch(self.command(&args)?, true, false).await?;
        Ok(())
    }

    /// List all the processes inside the container, returning their pids
    pub async fn ps(&self, id: &str) -> Result<Vec<usize>> {
        let args = [
            "ps".to_string(),
            "--format-json".to_string(),
            id.to_string(),
        ];
        let res = self.launch(self.command(&args)?, true, false).await?;
        let output = res.output.trim();
        // Ugly hack to work around golang
        Ok(if output == "null" {
            Vec::new()
        } else {
            serde_json::from_str(output).map_err(Error::JsonDeserializationFailed)?
        })
    }

    pub async fn restore(&self) -> Result<()> {
        Err(Error::Unimplemented("restore".to_string()))
    }

    /// Resume a container
    pub async fn resume(&self, id: &str) -> Result<()> {
        let args = ["pause".to_string(), id.to_string()];
        let _ = self.launch(self.command(&args)?, true, false).await?;
        Ok(())
    }

    /// Run the create, start, delete lifecycle of the container and return its exit status
    pub async fn run<P>(&self, id: &str, bundle: P, opts: Option<&CreateOpts>) -> Result<()>
    where
        P: AsRef<Path>,
    {
        let mut args = vec!["run".to_string(), "--bundle".to_string()];
        if let Some(opts) = opts {
            args.append(&mut opts.args()?);
        }
        args.push(utils::abs_string(bundle)?);
        args.push(id.to_string());
        let _ = self.launch(self.command(&args)?, true, false).await?;
        Ok(())
    }

    /// Start an already created container
    pub async fn start(&self, id: &str) -> Result<()> {
        let args = vec!["start".to_string(), id.to_string()];
        let _ = self.launch(self.command(&args)?, true, false).await?;
        Ok(())
    }

    /// Return the state of a container
    pub async fn state(&self, id: &str) -> Result<Vec<usize>> {
        let args = vec!["state".to_string(), id.to_string()];
        let res = self.launch(self.command(&args)?, true, false).await?;
        Ok(serde_json::from_str(&res.output).map_err(Error::JsonDeserializationFailed)?)
    }

    /// Return the latest statistics for a container
    pub async fn stats(&self, id: &str) -> Result<Stats> {
        let args = vec!["events".to_string(), "--stats".to_string(), id.to_string()];
        let res = self.launch(self.command(&args)?, true, false).await?;
        let event: Event =
            serde_json::from_str(&res.output).map_err(Error::JsonDeserializationFailed)?;
        if let Some(stats) = event.stats {
            Ok(stats)
        } else {
            Err(Error::MissingContainerStats)
        }
    }

    /// Update a container with the provided resource spec
    pub async fn update(&self, id: &str, resources: &LinuxResources) -> Result<()> {
        let (mut temp_file, file_name): (NamedTempFile, String) =
            utils::make_temp_file_in_runtime_dir()?;
        {
            let f = temp_file.as_file_mut();
            let spec_json =
                serde_json::to_string(resources).map_err(Error::JsonDeserializationFailed)?;
            f.write(spec_json.as_bytes())
                .map_err(Error::SpecFileCreationError)?;
            f.flush().map_err(Error::SpecFileCreationError)?;
        }
        let args = vec![
            "update".to_string(),
            "--resources".to_string(),
            file_name,
            id.to_string(),
        ];
        let _ = self.launch(self.command(&args)?, true, false).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // following style of go-runc, use only true/false to test
    const CMD_TRUE: &str = "/bin/true";
    const CMD_FALSE: &str = "/bin/false";

    fn ok_client() -> RuncClient {
        RuncConfig::new()
            .command(CMD_TRUE)
            .build()
            .expect("unable to create runc instance")
    }

    fn fail_client() -> RuncClient {
        RuncConfig::new()
            .command(CMD_FALSE)
            .build()
            .expect("unable to create runc instance")
    }

    fn ok_async_client() -> RuncAsyncClient {
        RuncConfig::new()
            .command(CMD_TRUE)
            .build_async()
            .expect("unable to create runc instance")
    }

    fn fail_async_client() -> RuncAsyncClient {
        RuncConfig::new()
            .command(CMD_FALSE)
            .build_async()
            .expect("unable to create runc instance")
    }

    fn dummy_process() -> Process {
        serde_json::from_str("{}").unwrap()
    }

    #[test]
    fn test_create() {
        let opts = CreateOpts::new();
        let ok_runc = ok_client();
        ok_runc
            .create("fake-id", "fake-bundle", Some(&opts))
            .expect("true failed.");
        eprintln!("ok_runc succeeded.");
        let fail_runc = fail_client();
        match fail_runc.create("fake-id", "fake-bundle", Some(&opts)) {
            Ok(_) => panic!("fail_runc returned exit status 0."),
            Err(Error::CommandFailed {
                status,
                stdout,
                stderr,
            }) => {
                if status.code().unwrap() == 1 && stdout.is_empty() && stderr.is_empty() {
                    eprintln!("fail_runc succeeded.");
                } else {
                    panic!("unexpected outputs from fail_runc.")
                }
            }
            Err(e) => panic!("unexpected error from fail_runc: {:?}", e),
        }
    }

    #[test]
    fn test_run() {
        let opts = CreateOpts::new();
        let ok_runc = ok_client();
        ok_runc
            .run("fake-id", "fake-bundle", Some(&opts))
            .expect("true failed.");
        eprintln!("ok_runc succeeded.");
        let fail_runc = fail_client();
        match fail_runc.run("fake-id", "fake-bundle", Some(&opts)) {
            Ok(_) => panic!("fail_runc returned exit status 0."),
            Err(Error::CommandFailed {
                status,
                stdout,
                stderr,
            }) => {
                if status.code().unwrap() == 1 && stdout.is_empty() && stderr.is_empty() {
                    eprintln!("fail_runc succeeded.");
                } else {
                    panic!("unexpected outputs from fail_runc.")
                }
            }
            Err(e) => panic!("unexpected error from fail_runc: {:?}", e),
        }
    }

    #[test]
    fn test_exec() {
        let opts = ExecOpts::new();
        let ok_runc = ok_client();
        let proc = dummy_process();
        ok_runc
            .exec("fake-id", &proc, Some(&opts))
            .expect("true failed.");
        eprintln!("ok_runc succeeded.");
        let fail_runc = fail_client();
        match fail_runc.exec("fake-id", &proc, Some(&opts)) {
            Ok(_) => panic!("fail_runc returned exit status 0."),
            Err(Error::CommandFailed {
                status,
                stdout,
                stderr,
            }) => {
                if status.code().unwrap() == 1 && stdout.is_empty() && stderr.is_empty() {
                    eprintln!("fail_runc succeeded.");
                } else {
                    panic!("unexpected outputs from fail_runc.")
                }
            }
            Err(e) => panic!("unexpected error from fail_runc: {:?}", e),
        }
    }

    #[test]
    fn test_delete() {
        let opts = DeleteOpts::new();
        let ok_runc = ok_client();
        ok_runc
            .delete("fake-id", Some(&opts))
            .expect("true failed.");
        eprintln!("ok_runc succeeded.");
        let fail_runc = fail_client();
        match fail_runc.delete("fake-id", Some(&opts)) {
            Ok(_) => panic!("fail_runc returned exit status 0."),
            Err(Error::CommandFailed {
                status,
                stdout,
                stderr,
            }) => {
                if status.code().unwrap() == 1 && stdout.is_empty() && stderr.is_empty() {
                    eprintln!("fail_runc succeeded.");
                } else {
                    panic!("unexpected outputs from fail_runc.")
                }
            }
            Err(e) => panic!("unexpected error from fail_runc: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_async_create() {
        let opts = CreateOpts::new();
        let ok_runc = ok_async_client();
        let ok_task = tokio::spawn(async move {
            ok_runc
                .create("fake-id", "fake-bundle", Some(&opts))
                .await
                .expect("true failed.");
            eprintln!("ok_runc succeeded.");
        });

        let opts = CreateOpts::new();
        let fail_runc = fail_async_client();
        let fail_task = tokio::spawn(async move {
            match fail_runc
                .create("fake-id", "fake-bundle", Some(&opts))
                .await
            {
                Ok(_) => panic!("fail_runc returned exit status 0."),
                Err(Error::CommandFailed {
                    status,
                    stdout,
                    stderr,
                }) => {
                    if status.code().unwrap() == 1 && stdout.is_empty() && stderr.is_empty() {
                        eprintln!("fail_runc succeeded.");
                    } else {
                        panic!("unexpected outputs from fail_runc.")
                    }
                }
                Err(e) => panic!("unexpected error from fail_runc: {:?}", e),
            }
        });

        ok_task.await.expect("ok_task failed.");
        fail_task.await.expect("fail_task unexpectedly succeeded.");
    }

    #[tokio::test]
    async fn test_async_start() {
        let ok_runc = ok_async_client();
        let ok_task = tokio::spawn(async move {
            ok_runc.start("fake-id").await.expect("true failed.");
            eprintln!("ok_runc succeeded.");
        });

        let fail_runc = fail_async_client();
        let fail_task = tokio::spawn(async move {
            match fail_runc.start("fake-id").await {
                Ok(_) => panic!("fail_runc returned exit status 0."),
                Err(Error::CommandFailed {
                    status,
                    stdout,
                    stderr,
                }) => {
                    if status.code().unwrap() == 1 && stdout.is_empty() && stderr.is_empty() {
                        eprintln!("fail_runc succeeded.");
                    } else {
                        panic!("unexpected outputs from fail_runc.")
                    }
                }
                Err(e) => panic!("unexpected error from fail_runc: {:?}", e),
            }
        });

        ok_task.await.expect("ok_task failed.");
        fail_task.await.expect("fail_task unexpectedly succeeded.");
    }

    #[tokio::test]
    async fn test_async_run() {
        let opts = CreateOpts::new();
        let ok_runc = RuncConfig::new()
            .command(CMD_TRUE)
            .build_async()
            .expect("unable to create runc instance");
        tokio::spawn(async move {
            ok_runc
                .create("fake-id", "fake-bundle", Some(&opts))
                .await
                .expect("true failed.");
            eprintln!("ok_runc succeeded.");
        });

        let opts = CreateOpts::new();
        let fail_runc = RuncConfig::new()
            .command(CMD_FALSE)
            .build_async()
            .expect("unable to create runc instance");
        tokio::spawn(async move {
            match fail_runc
                .create("fake-id", "fake-bundle", Some(&opts))
                .await
            {
                Ok(_) => panic!("fail_runc returned exit status 0."),
                Err(Error::CommandFailed {
                    status,
                    stdout,
                    stderr,
                }) => {
                    if status.code().unwrap() == 1 && stdout.is_empty() && stderr.is_empty() {
                        eprintln!("fail_runc succeeded.");
                    } else {
                        panic!("unexpected outputs from fail_runc.")
                    }
                }
                Err(e) => panic!("unexpected error from fail_runc: {:?}", e),
            }
        })
        .await
        .expect("tokio spawn falied.");
    }

    #[tokio::test]
    async fn test_async_delete() {
        let opts = DeleteOpts::new();
        let ok_runc = RuncConfig::new()
            .command(CMD_TRUE)
            .build_async()
            .expect("unable to create runc instance");
        tokio::spawn(async move {
            ok_runc
                .delete("fake-id", Some(&opts))
                .await
                .expect("true failed.");
            eprintln!("ok_runc succeeded.");
        });

        let opts = DeleteOpts::new();
        let fail_runc = RuncConfig::new()
            .command(CMD_FALSE)
            .build_async()
            .expect("unable to create runc instance");
        tokio::spawn(async move {
            match fail_runc.delete("fake-id", Some(&opts)).await {
                Ok(_) => panic!("fail_runc returned exit status 0."),
                Err(Error::CommandFailed {
                    status,
                    stdout,
                    stderr,
                }) => {
                    if status.code().unwrap() == 1 && stdout.is_empty() && stderr.is_empty() {
                        eprintln!("fail_runc succeeded.");
                    } else {
                        panic!("unexpected outputs from fail_runc.")
                    }
                }
                Err(e) => panic!("unexpected error from fail_runc: {:?}", e),
            }
        })
        .await
        .expect("tokio spawn falied.");
    }
}
