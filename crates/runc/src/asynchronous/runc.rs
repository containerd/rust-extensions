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

use std::{fmt::Debug, path::Path, process::ExitStatus};

use async_trait::async_trait;
use log::debug;
use oci_spec::runtime::{LinuxResources, Process};

use crate::{
    container::Container,
    error::Error,
    events,
    options::*,
    utils::{self, write_value_to_temp_file},
    Command, Response, Result, Runc, RuncGlobalArgs,
};

// a macro tool to cleanup the file with name $filename,
// there is no async drop in async rust, so we have to call remove_file everytime
// after a temp file created, before return of a function.
// with this macro we don't have to write the match case codes everytime.
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

/// Async implementation for [Runc].
///
/// Note that you MUST use this client on tokio runtime, as this client internally use [`tokio::process::Command`]
/// and some other utilities.
impl Runc {
    pub(crate) async fn launch(&self, mut cmd: Command, combined_output: bool) -> Result<Response> {
        debug!("Execute command {:?}", cmd);
        unsafe {
            cmd.pre_exec(move || {
                #[cfg(target_os = "linux")]
                if let Ok(thp) = std::env::var("THP_DISABLED") {
                    if let Ok(thp_disabled) = thp.parse::<bool>() {
                        if let Err(e) = prctl::set_thp_disable(thp_disabled) {
                            debug!("set_thp_disable err: {}", e);
                        };
                    }
                }
                Ok(())
            });
        }

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
                io.set(&mut cmd).await.map_err(Error::UnavailableIO)?;
                let res = self.launch(cmd, true).await?;
                io.close_after_start().await;
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
        let mut custom_global_args = RuncGlobalArgs {
            ..Default::default()
        };
        if let Some(opts) = opts {
            args.append(&mut tc!(opts.args(), &f));
            custom_global_args = opts.custom_args.clone();
        }
        args.push(id.to_string());
        let mut cmd = self.command_with_global_args(&args, custom_global_args)?;
        match opts {
            Some(ExecOpts { io: Some(io), .. }) => {
                tc!(
                    io.set(&mut cmd)
                        .await
                        .map_err(|e| Error::IoSet(e.to_string())),
                    &f
                );
                tc!(self.launch(cmd, true).await, &f);
                io.close_after_start().await;
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
            io.set(&mut cmd)
                .await
                .map_err(|e| Error::IoSet(e.to_string()))?;
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
    pub async fn state(&self, id: &str) -> Result<Container> {
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

#[async_trait]
pub trait Spawner: Debug {
    async fn execute(&self, cmd: Command) -> Result<(ExitStatus, u32, String, String)>;
}

#[derive(Debug)]
pub struct DefaultExecutor {}

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

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use std::sync::Arc;

    use crate::{
        error::Error,
        io::{InheritedStdIo, PipedStdIo},
        options::{CreateOpts, DeleteOpts, GlobalOpts},
        Runc,
    };

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
