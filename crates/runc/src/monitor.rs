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

use std::process::{ExitStatus, Output};

use async_trait::async_trait;
use log::error;
use time::OffsetDateTime;
use tokio::process::Command;
use tokio::sync::oneshot::{channel, Receiver, Sender};

use crate::error::Error;

/// A trait for spawning and waiting for a process.
///
/// The design is different from Go's, because if you return a `Sender` in [ProcessMonitor::start()]
/// and want to use it in [ProcessMonitor::wait()], then start and wait cannot be executed
/// concurrently. Alternatively, let the caller to prepare the communication channel for
/// [ProcessMonitor::start()] and [ProcessMonitor::wait()] so they could be executed concurrently.
#[async_trait]
pub trait ProcessMonitor {
    /// Spawn a process and return its output.
    ///
    /// In order to capture the output/error, it is necessary for the caller to create new pipes
    /// between parent and child.
    /// Use [tokio::process::Command::stdout(Stdio::piped())](https://docs.rs/tokio/1.16.1/tokio/process/struct.Command.html#method.stdout)
    /// and/or [tokio::process::Command::stderr(Stdio::piped())](https://docs.rs/tokio/1.16.1/tokio/process/struct.Command.html#method.stderr)
    /// respectively, when creating the [Command](https://docs.rs/tokio/1.16.1/tokio/process/struct.Command.html#).
    async fn start(&self, mut cmd: Command, tx: Sender<Exit>) -> std::io::Result<Output> {
        let chi = cmd.spawn()?;
        // Safe to expect() because wait() hasn't been called yet, dependence on tokio interanl
        // implementation details.
        let pid = chi
            .id()
            .expect("failed to take pid of the container process.");
        let out = chi.wait_with_output().await?;
        let ts = OffsetDateTime::now_utc();
        // On Unix, out.status.code() will return None if the process was terminated by a signal.
        let status = out.status.code().unwrap_or(-1);
        match tx.send(Exit { ts, pid, status }) {
            Ok(_) => Ok(out),
            Err(e) => {
                error!("command {:?} exited but receiver dropped.", cmd);
                error!("couldn't send messages: {:?}", e);
                Err(std::io::ErrorKind::ConnectionRefused.into())
            }
        }
    }

    /// Wait for the spawned process to exit and return the exit status.
    async fn wait(&self, rx: Receiver<Exit>) -> std::io::Result<Exit> {
        rx.await.map_err(|_| {
            error!("sender dropped.");
            std::io::ErrorKind::BrokenPipe.into()
        })
    }
}

/// A default implementation of [ProcessMonitor].
#[derive(Debug, Clone, Default)]
pub struct DefaultMonitor {}

impl ProcessMonitor for DefaultMonitor {}

impl DefaultMonitor {
    pub const fn new() -> Self {
        Self {}
    }
}

/// Process exit status returned by [ProcessMonitor::wait()].
#[derive(Debug)]
pub struct Exit {
    pub ts: OffsetDateTime,
    pub pid: u32,
    pub status: i32,
}

/// Execution result returned by `execute()`.
pub struct ExecuteResult {
    pub exit: Exit,
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// Execute a `Command` and collect exit status, output and error messages.
///
/// To collect output and error messages, pipes must be used for Command's stdout and stderr.
///
/// Note: invalid UTF-8 characters in output and error messages will be replaced with the `�` char.
pub async fn execute<T: ProcessMonitor + Send + Sync>(
    monitor: &T,
    cmd: Command,
) -> Result<ExecuteResult, Error> {
    let (tx, rx) = channel::<Exit>();
    let start = monitor.start(cmd, tx);
    let wait = monitor.wait(rx);
    let (
        Output {
            stdout,
            stderr,
            status,
        },
        exit,
    ) = tokio::try_join!(start, wait).map_err(Error::InvalidCommand)?;
    let stdout = String::from_utf8_lossy(&stdout).to_string();
    let stderr = String::from_utf8_lossy(&stderr).to_string();

    Ok(ExecuteResult {
        exit,
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;

    use tokio::process::Command;
    use tokio::sync::oneshot::channel;

    use super::*;

    #[tokio::test]
    async fn test_start_wait_without_output() {
        let monitor = DefaultMonitor::new();
        let cmd = Command::new("/bin/ls");
        let (tx, rx) = channel();

        let output = monitor.start(cmd, tx).await.unwrap();
        assert_eq!(output.stdout.len(), 0);
        assert_eq!(output.stderr.len(), 0);
        let status = monitor.wait(rx).await.unwrap();
        assert_eq!(status.status, 0);
    }

    #[tokio::test]
    async fn test_start_wait_with_output() {
        let monitor = DefaultMonitor::new();
        let mut cmd = Command::new("/bin/ls");
        cmd.stdout(Stdio::piped());
        let (tx, rx) = channel();

        let output = monitor.start(cmd, tx).await.unwrap();
        assert!(!output.stdout.is_empty());
        assert_eq!(output.stderr.len(), 0);
        let status = monitor.wait(rx).await.unwrap();
        assert_eq!(status.status, 0);
    }

    #[tokio::test]
    async fn test_execute() {
        let mut cmd = Command::new("/bin/ls");
        cmd.stdout(Stdio::piped());
        let monitor = DefaultMonitor::new();
        let result = execute(&monitor, cmd).await.unwrap();

        assert_eq!(result.exit.status, 0);
        assert!(result.status.success());
        assert!(!result.stdout.is_empty());
        assert_eq!(result.stderr.len(), 0);
    }
}
