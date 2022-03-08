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

//! A crate for consuming the runc binary in your Rust applications, similar to
//! [go-runc](https://github.com/containerd/go-runc) for Go.
use std::fmt::{self, Debug, Display};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;

#[cfg(feature = "async")]
use async_trait::async_trait;
#[cfg(feature = "async")]
use log::debug;
use oci_spec::runtime::{LinuxResources, Process};

use crate::container::Container;
use crate::error::Error;
use crate::options::*;
use crate::utils::write_value_to_temp_file;

pub mod container;
pub mod error;
pub mod events;
pub mod io;
#[cfg(feature = "async")]
pub mod monitor;
pub mod options;
pub mod utils;

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

impl Default for LogFormat {
    fn default() -> Self {
        LogFormat::Text
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

#[cfg(not(feature = "async"))]
impl Runc {
    fn launch(&self, cmd: Command, combined_output: bool) -> Result<Response> {
        let (status, pid, stdout, stderr) = self.spawner.execute(cmd)?;
        if status.success() {
            let output = if combined_output {
                stdout + stderr.as_str()
            } else {
                stdout
            };
            Ok(Response {
                pid,
                status,
                output,
            })
        } else {
            Err(Error::CommandFailed {
                status,
                stdout,
                stderr,
            })
        }
    }

    /// Create a new container
    pub fn create<P>(&self, id: &str, bundle: P, opts: Option<&CreateOpts>) -> Result<Response>
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
            Some(CreateOpts { io: Some(io), .. }) => {
                io.set(&mut cmd).map_err(|e| Error::IoSet(e.to_string()))?;
                let res = self.launch(cmd, true)?;
                io.close_after_start();
                Ok(res)
            }
            _ => self.launch(cmd, true),
        }
    }

    /// Delete a container
    pub fn delete(&self, id: &str, opts: Option<&DeleteOpts>) -> Result<()> {
        let mut args = vec!["delete".to_string()];
        if let Some(opts) = opts {
            args.append(&mut opts.args());
        }
        args.push(id.to_string());
        self.launch(self.command(&args)?, true)?;
        Ok(())
    }

    /// Execute an additional process inside the container
    pub fn exec(&self, id: &str, spec: &Process, opts: Option<&ExecOpts>) -> Result<()> {
        let (_temp_file, filename) = write_value_to_temp_file(spec)?;
        let mut args = vec!["exec".to_string(), "--process".to_string(), filename];
        if let Some(opts) = opts {
            args.append(&mut opts.args()?);
        }
        args.push(id.to_string());
        let mut cmd = self.command(&args)?;
        match opts {
            Some(ExecOpts { io: Some(io), .. }) => {
                io.set(&mut cmd).map_err(|e| Error::IoSet(e.to_string()))?;
                self.launch(cmd, true)?;
                io.close_after_start();
            }
            _ => {
                self.launch(cmd, true)?;
            }
        }
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
        let _ = self.launch(self.command(&args)?, true)?;
        Ok(())
    }

    /// List all containers associated with this runc instance
    pub fn list(&self) -> Result<Vec<Container>> {
        let args = ["list".to_string(), "--format=json".to_string()];
        let res = self.launch(self.command(&args)?, true)?;
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
        let _ = self.launch(self.command(&args)?, true)?;
        Ok(())
    }

    /// Resume a container
    pub fn resume(&self, id: &str) -> Result<()> {
        let args = ["resume".to_string(), id.to_string()];
        let _ = self.launch(self.command(&args)?, true)?;
        Ok(())
    }

    pub fn checkpoint(&self) -> Result<()> {
        Err(Error::Unimplemented("checkpoint".to_string()))
    }

    pub fn restore(&self) -> Result<()> {
        Err(Error::Unimplemented("restore".to_string()))
    }

    /// List all the processes inside the container, returning their pids
    pub fn ps(&self, id: &str) -> Result<Vec<usize>> {
        let args = [
            "ps".to_string(),
            "--format=json".to_string(),
            id.to_string(),
        ];
        let res = self.launch(self.command(&args)?, false)?;
        let output = res.output.trim();

        // Ugly hack to work around golang
        Ok(if output == "null" {
            Vec::new()
        } else {
            serde_json::from_str(output).map_err(Error::JsonDeserializationFailed)?
        })
    }

    /// Run the create, start, delete lifecycle of the container and return its exit status
    pub fn run<P>(&self, id: &str, bundle: P, opts: Option<&CreateOpts>) -> Result<Response>
    where
        P: AsRef<Path>,
    {
        let mut args = vec![
            "run".to_string(),
            "--bundle".to_string(),
            utils::abs_string(bundle)?,
        ];
        if let Some(opts) = opts {
            args.append(&mut opts.args()?);
        }
        args.push(id.to_string());
        let mut cmd = self.command(&args)?;
        if let Some(CreateOpts { io: Some(io), .. }) = opts {
            io.set(&mut cmd).map_err(|e| Error::IoSet(e.to_string()))?;
        };
        self.launch(cmd, true)
    }

    /// Start an already created container
    pub fn start(&self, id: &str) -> Result<Response> {
        let args = ["start".to_string(), id.to_string()];
        self.launch(self.command(&args)?, true)
    }

    /// Return the state of a container
    pub fn state(&self, id: &str) -> Result<Container> {
        let args = ["state".to_string(), id.to_string()];
        let res = self.launch(self.command(&args)?, true)?;
        serde_json::from_str(&res.output).map_err(Error::JsonDeserializationFailed)
    }

    /// Return the latest statistics for a container
    pub fn stats(&self, id: &str) -> Result<events::Stats> {
        let args = vec!["events".to_string(), "--stats".to_string(), id.to_string()];
        let res = self.launch(self.command(&args)?, true)?;
        let event: events::Event =
            serde_json::from_str(&res.output).map_err(Error::JsonDeserializationFailed)?;
        if let Some(stats) = event.stats {
            Ok(stats)
        } else {
            Err(Error::MissingContainerStats)
        }
    }

    /// Update a container with the provided resource spec
    pub fn update(&self, id: &str, resources: &LinuxResources) -> Result<()> {
        let (_temp_file, filename) = write_value_to_temp_file(resources)?;
        let args = [
            "update".to_string(),
            "--resources".to_string(),
            filename,
            id.to_string(),
        ];
        self.launch(self.command(&args)?, true)?;
        Ok(())
    }
}

// a macro tool to cleanup the file with name $filename,
// there is no async drop in async rust, so we have to call remove_file everytime
// after a temp file created, before return of a function.
// with this macro we don't have to write the match case codes everytime.
#[cfg(feature = "async")]
macro_rules! tc {
    ($b:expr, $filename: expr) => {
        match $b {
            Ok(r) => r,
            Err(e) => {
                let _ = tokio::fs::remove_file($filename).await;
                return Err(e);
            }
        }
    };
}

#[cfg(not(feature = "async"))]
pub trait Spawner: Debug {
    fn execute(&self, cmd: Command) -> Result<(ExitStatus, u32, String, String)>;
}

#[cfg(feature = "async")]
#[async_trait]
pub trait Spawner: Debug {
    async fn execute(&self, cmd: Command) -> Result<(ExitStatus, u32, String, String)>;
}

/// Async implementation for [Runc].
///
/// Note that you MUST use this client on tokio runtime, as this client internally use [`tokio::process::Command`]
/// and some other utilities.
#[cfg(feature = "async")]
impl Runc {
    async fn launch(&self, cmd: Command, combined_output: bool) -> Result<Response> {
        debug!("Execute command {:?}", cmd);
        let (status, pid, stdout, stderr) = self.spawner.execute(cmd).await?;
        if status.success() {
            let output = if combined_output {
                stdout + stderr.as_str()
            } else {
                stdout
            };
            Ok(Response {
                pid,
                status,
                output,
            })
        } else {
            Err(Error::CommandFailed {
                status,
                stdout,
                stderr,
            })
        }
    }

    /// Create a new container
    pub async fn create<P>(
        &self,
        id: &str,
        bundle: P,
        opts: Option<&CreateOpts>,
    ) -> Result<Response>
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
            Some(CreateOpts { io: Some(io), .. }) => {
                io.set(&mut cmd).map_err(Error::UnavailableIO)?;
                let res = self.launch(cmd, true).await?;
                io.close_after_start();
                Ok(res)
            }
            _ => self.launch(cmd, true).await,
        }
    }

    /// Delete a container
    pub async fn delete(&self, id: &str, opts: Option<&DeleteOpts>) -> Result<()> {
        let mut args = vec!["delete".to_string()];
        if let Some(opts) = opts {
            args.append(&mut opts.args());
        }
        args.push(id.to_string());
        let _ = self.launch(self.command(&args)?, true).await?;
        Ok(())
    }

    /// Return an event stream of container notifications
    pub async fn events(&self, _id: &str, _interval: &std::time::Duration) -> Result<()> {
        Err(Error::Unimplemented("events".to_string()))
    }

    /// Execute an additional process inside the container
    pub async fn exec(&self, id: &str, spec: &Process, opts: Option<&ExecOpts>) -> Result<()> {
        let f = write_value_to_temp_file(spec).await?;
        let mut args = vec!["exec".to_string(), "--process".to_string(), f.clone()];
        if let Some(opts) = opts {
            args.append(&mut tc!(opts.args(), &f));
        }
        args.push(id.to_string());
        let mut cmd = self.command(&args)?;
        match opts {
            Some(ExecOpts { io: Some(io), .. }) => {
                tc!(
                    io.set(&mut cmd).map_err(|e| Error::IoSet(e.to_string())),
                    &f
                );
                tc!(self.launch(cmd, true).await, &f);
                io.close_after_start();
            }
            _ => {
                tc!(self.launch(cmd, true).await, &f);
            }
        }
        let _ = tokio::fs::remove_file(&f).await;
        Ok(())
    }

    /// Send the specified signal to processes inside the container
    pub async fn kill(&self, id: &str, sig: u32, opts: Option<&KillOpts>) -> Result<()> {
        let mut args = vec!["kill".to_string()];
        if let Some(opts) = opts {
            args.append(&mut opts.args());
        }
        args.push(id.to_string());
        args.push(sig.to_string());
        let _ = self.launch(self.command(&args)?, true).await?;
        Ok(())
    }

    /// List all containers associated with this runc instance
    pub async fn list(&self) -> Result<Vec<Container>> {
        let args = ["list".to_string(), "--format=json".to_string()];
        let res = self.launch(self.command(&args)?, true).await?;
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
        let _ = self.launch(self.command(&args)?, true).await?;
        Ok(())
    }

    /// Resume a container
    pub async fn resume(&self, id: &str) -> Result<()> {
        let args = ["resume".to_string(), id.to_string()];
        let _ = self.launch(self.command(&args)?, true).await?;
        Ok(())
    }

    pub async fn checkpoint(&self) -> Result<()> {
        Err(Error::Unimplemented("checkpoint".to_string()))
    }

    pub async fn restore(&self) -> Result<()> {
        Err(Error::Unimplemented("restore".to_string()))
    }

    /// List all the processes inside the container, returning their pids
    pub async fn ps(&self, id: &str) -> Result<Vec<usize>> {
        let args = [
            "ps".to_string(),
            "--format=json".to_string(),
            id.to_string(),
        ];
        let res = self.launch(self.command(&args)?, true).await?;
        let output = res.output.trim();

        // Ugly hack to work around golang
        Ok(if output == "null" {
            Vec::new()
        } else {
            serde_json::from_str(output).map_err(Error::JsonDeserializationFailed)?
        })
    }

    /// Run the create, start, delete lifecycle of the container and return its exit status
    pub async fn run<P>(&self, id: &str, bundle: P, opts: Option<&CreateOpts>) -> Result<()>
    where
        P: AsRef<Path>,
    {
        let mut args = vec![
            "run".to_string(),
            "--bundle".to_string(),
            utils::abs_string(bundle)?,
        ];
        if let Some(opts) = opts {
            args.append(&mut opts.args()?);
        }
        args.push(id.to_string());
        let mut cmd = self.command(&args)?;
        if let Some(CreateOpts { io: Some(io), .. }) = opts {
            io.set(&mut cmd).map_err(|e| Error::IoSet(e.to_string()))?;
        };
        let _ = self.launch(cmd, true).await?;
        Ok(())
    }

    /// Start an already created container
    pub async fn start(&self, id: &str) -> Result<()> {
        let args = vec!["start".to_string(), id.to_string()];
        let _ = self.launch(self.command(&args)?, true).await?;
        Ok(())
    }

    /// Return the state of a container
    pub async fn state(&self, id: &str) -> Result<Vec<usize>> {
        let args = vec!["state".to_string(), id.to_string()];
        let res = self.launch(self.command(&args)?, true).await?;
        serde_json::from_str(&res.output).map_err(Error::JsonDeserializationFailed)
    }

    /// Return the latest statistics for a container
    pub async fn stats(&self, id: &str) -> Result<events::Stats> {
        let args = vec!["events".to_string(), "--stats".to_string(), id.to_string()];
        let res = self.launch(self.command(&args)?, true).await?;
        let event: events::Event =
            serde_json::from_str(&res.output).map_err(Error::JsonDeserializationFailed)?;
        if let Some(stats) = event.stats {
            Ok(stats)
        } else {
            Err(Error::MissingContainerStats)
        }
    }

    /// Update a container with the provided resource spec
    pub async fn update(&self, id: &str, resources: &LinuxResources) -> Result<()> {
        let f = write_value_to_temp_file(resources).await?;
        let args = [
            "update".to_string(),
            "--resources".to_string(),
            f.to_string(),
            id.to_string(),
        ];
        let _ = tc!(self.launch(self.command(&args)?, true).await, &f);
        let _ = tokio::fs::remove_file(&f).await;
        Ok(())
    }
}

#[cfg(test)]
#[cfg(all(target_os = "linux", not(feature = "async")))]
mod tests {
    use std::sync::Arc;

    use super::io::{InheritedStdIo, PipedStdIo};
    use super::*;

    fn ok_client() -> Runc {
        GlobalOpts::new()
            .command("/bin/true")
            .build()
            .expect("unable to create runc instance")
    }

    fn fail_client() -> Runc {
        GlobalOpts::new()
            .command("/bin/false")
            .build()
            .expect("unable to create runc instance")
    }

    fn echo_client() -> Runc {
        GlobalOpts::new()
            .command("/bin/echo")
            .build()
            .expect("unable to create runc instance")
    }

    fn dummy_process() -> Process {
        serde_json::from_str(
            "
            {
                \"user\": {
                    \"uid\": 1000,
                    \"gid\": 1000
                },
                \"cwd\": \"/path/to/dir\"
            }",
        )
        .unwrap()
    }

    #[test]
    fn test_create() {
        let opts = CreateOpts::new();
        let ok_runc = ok_client();
        let response = ok_runc
            .create("fake-id", "fake-bundle", Some(&opts))
            .expect("true failed.");
        assert_ne!(response.pid, 0);
        assert!(response.status.success());
        assert!(response.output.is_empty());

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
        let response = ok_runc
            .run("fake-id", "fake-bundle", Some(&opts))
            .expect("true failed.");
        assert_ne!(response.pid, 0);
        assert!(response.status.success());
        assert!(response.output.is_empty());

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

    #[test]
    fn test_output() {
        // test create cmd with inherit Io, expect empty cmd output
        let mut opts = CreateOpts::new();
        opts.io = Some(Arc::new(InheritedStdIo::new().unwrap()));
        let echo_runc = echo_client();
        let response = echo_runc
            .create("fake-id", "fake-bundle", Some(&opts))
            .expect("echo failed.");
        assert_ne!(response.pid, 0);
        assert!(response.status.success());
        assert!(response.output.is_empty());

        // test create cmd with pipe Io, expect nonempty cmd output
        let mut opts = CreateOpts::new();
        opts.io = Some(Arc::new(PipedStdIo::new().unwrap()));
        let echo_runc = echo_client();
        let response = echo_runc
            .create("fake-id", "fake-bundle", Some(&opts))
            .expect("echo failed.");
        assert_ne!(response.pid, 0);
        assert!(response.status.success());
        assert!(!response.output.is_empty());
    }
}

/// Tokio tests
#[cfg(test)]
#[cfg(all(target_os = "linux", feature = "async"))]
mod tests {
    use std::sync::Arc;

    use super::io::{InheritedStdIo, PipedStdIo};
    use super::*;

    fn ok_client() -> Runc {
        GlobalOpts::new()
            .command("/bin/true")
            .build()
            .expect("unable to create runc instance")
    }

    fn fail_client() -> Runc {
        GlobalOpts::new()
            .command("/bin/false")
            .build()
            .expect("unable to create runc instance")
    }

    fn echo_client() -> Runc {
        GlobalOpts::new()
            .command("/bin/echo")
            .build()
            .expect("unable to create runc instance")
    }

    #[tokio::test]
    async fn test_async_create() {
        let opts = CreateOpts::new();
        let ok_runc = ok_client();
        let ok_task = tokio::spawn(async move {
            let response = ok_runc
                .create("fake-id", "fake-bundle", Some(&opts))
                .await
                .expect("true failed.");
            assert_ne!(response.pid, 0);
            assert!(response.status.success());
            assert!(response.output.is_empty());
        });

        let opts = CreateOpts::new();
        let fail_runc = fail_client();
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
        let ok_runc = ok_client();
        let ok_task = tokio::spawn(async move {
            ok_runc.start("fake-id").await.expect("true failed.");
            eprintln!("ok_runc succeeded.");
        });

        let fail_runc = fail_client();
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
        let ok_runc = ok_client();
        tokio::spawn(async move {
            ok_runc
                .create("fake-id", "fake-bundle", Some(&opts))
                .await
                .expect("true failed.");
            eprintln!("ok_runc succeeded.");
        });

        let opts = CreateOpts::new();
        let fail_runc = fail_client();
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
        let ok_runc = ok_client();
        tokio::spawn(async move {
            ok_runc
                .delete("fake-id", Some(&opts))
                .await
                .expect("true failed.");
            eprintln!("ok_runc succeeded.");
        });

        let opts = DeleteOpts::new();
        let fail_runc = fail_client();
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

    #[tokio::test]
    async fn test_async_output() {
        // test create cmd with inherit Io, expect empty cmd output
        let mut opts = CreateOpts::new();
        opts.io = Some(Arc::new(InheritedStdIo::new().unwrap()));
        let echo_runc = echo_client();
        let response = echo_runc
            .create("fake-id", "fake-bundle", Some(&opts))
            .await
            .expect("echo failed:");
        assert_ne!(response.pid, 0);
        assert!(response.status.success());
        assert!(response.output.is_empty());

        // test create cmd with pipe Io, expect nonempty cmd output
        let mut opts = CreateOpts::new();
        opts.io = Some(Arc::new(PipedStdIo::new().unwrap()));
        let response = echo_runc
            .create("fake-id", "fake-bundle", Some(&opts))
            .await
            .expect("echo failed:");
        assert_ne!(response.pid, 0);
        assert!(response.status.success());
        assert!(!response.output.is_empty());
    }
}

#[derive(Debug)]
pub struct DefaultExecutor {}

#[cfg(feature = "async")]
#[async_trait]
impl Spawner for DefaultExecutor {
    async fn execute(&self, cmd: Command) -> Result<(ExitStatus, u32, String, String)> {
        let mut cmd = cmd;
        let child = cmd.spawn().map_err(Error::ProcessSpawnFailed)?;
        let pid = child.id().unwrap();
        let result = child
            .wait_with_output()
            .await
            .map_err(Error::InvalidCommand)?;
        let status = result.status;
        let stdout = String::from_utf8_lossy(&result.stdout).to_string();
        let stderr = String::from_utf8_lossy(&result.stderr).to_string();
        Ok((status, pid, stdout, stderr))
    }
}

#[cfg(not(feature = "async"))]
impl Spawner for DefaultExecutor {
    fn execute(&self, cmd: Command) -> Result<(ExitStatus, u32, String, String)> {
        let mut cmd = cmd;
        let child = cmd.spawn().map_err(Error::ProcessSpawnFailed)?;
        let pid = child.id();
        let result = child.wait_with_output().map_err(Error::InvalidCommand)?;
        let status = result.status;
        let stdout = String::from_utf8_lossy(&result.stdout).to_string();
        let stderr = String::from_utf8_lossy(&result.stderr).to_string();
        Ok((status, pid, stdout, stderr))
    }
}
