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

//! A process's stdio, copied between the runtime and containerd's fifos.

use std::{io::ErrorKind, os::unix::fs::OpenOptionsExt, time::Duration};

use log::{debug, error, warn};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::unix::pipe,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    error::{Error, Result},
    io_error,
};

/// How long a deleted process's output copies may keep running to deliver
/// what is still buffered. The Go shim allows the same.
const IO_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// The stdio containerd asked a process to have: fifo paths, empty when not
/// wanted, and whether the process gets a terminal.
#[derive(Clone, Debug, Default)]
pub struct Stdio {
    pub stdin: String,
    pub stdout: String,
    pub stderr: String,
    pub terminal: bool,
}

impl Stdio {
    pub fn new(stdin: &str, stdout: &str, stderr: &str, terminal: bool) -> Self {
        Self {
            stdin: stdin.to_string(),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            terminal,
        }
    }

    pub fn is_null(&self) -> bool {
        self.stdin.is_empty() && self.stdout.is_empty() && self.stderr.is_empty()
    }
}

/// The tasks copying a process's stdio between the runtime and containerd's
/// fifos, which a shim shuts down once the process is deleted.
#[derive(Clone)]
pub struct IoTasks {
    outputs: TaskTracker,
    inputs: TaskTracker,
    cancel: CancellationToken,
    /// Cancelled with `cancel`, or alone to close the process's stdin.
    cancel_inputs: CancellationToken,
}

impl Default for IoTasks {
    fn default() -> Self {
        let cancel = CancellationToken::new();
        Self {
            outputs: TaskTracker::new(),
            inputs: TaskTracker::new(),
            cancel_inputs: cancel.child_token(),
            cancel,
        }
    }
}

impl IoTasks {
    /// Copies the process's input from the fifo at `path` `to` the runtime.
    ///
    /// The fifo is opened without waiting for a writer, as the Go shim does:
    /// the reactor delivers nothing until one connects, and EOF once the last
    /// one leaves, so the process does not see a premature EOF either.
    /// Shutdown cancels the copy without draining it, as containerd commonly
    /// keeps a stdin fifo open past delete.
    pub fn copy_from_fifo<W>(&self, path: &str, to: W) -> Result<()>
    where
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let fifo = pipe::OpenOptions::new()
            .open_receiver(path)
            .map_err(io_error!(e, "open {}", path))?;
        self.inputs
            .spawn(Self::copy(self.cancel_inputs.clone(), fifo, to));
        Ok(())
    }

    /// Copies the process's output `from` the runtime to the fifo at `path`
    /// until either end closes or the tasks are shut down.
    ///
    /// The fifo is held open for reading as well, so the copy outlives a
    /// containerd restart instead of failing with EPIPE. Neither open waits:
    /// the shim's own reader is in place before the writer opens, so the
    /// output just queues in the fifo until containerd reads it.
    pub fn copy_to_fifo<R>(&self, from: R, path: &str) -> Result<()>
    where
        R: AsyncRead + Send + Unpin + 'static,
    {
        let keep = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(io_error!(e, "open {} for read", path))?;
        // Driven by the reactor rather than the blocking pool: a parked write
        // costs no thread, and dropping the writer releases the fifo at once.
        let fifo = pipe::OpenOptions::new()
            .open_sender(path)
            .map_err(io_error!(e, "open {}", path))?;
        let copy = Self::copy(self.cancel.clone(), from, fifo);
        self.outputs.spawn(async move {
            let _keep = keep;
            copy.await;
        });
        Ok(())
    }

    /// Closes the process's stdin, as CloseIO asks, by ending the input
    /// copies: dropping their end of the pipe is what the process sees as EOF.
    pub async fn close_input(&self) {
        self.cancel_inputs.cancel();
        self.inputs.close();
        self.inputs.wait().await;
    }

    /// Lets the output copies finish for up to the drain timeout, then cancels
    /// whatever still runs: the input copies, and output copies blocked on a
    /// pipe some other process holds open. That releases the fifos to
    /// containerd.
    pub async fn shutdown(&self) {
        self.shutdown_within(IO_DRAIN_TIMEOUT).await
    }

    async fn shutdown_within(&self, drain: Duration) {
        self.outputs.close();
        if tokio::time::timeout(drain, self.outputs.wait())
            .await
            .is_err()
        {
            warn!(
                "output copy did not finish within {:?}, cancelling it",
                drain
            );
        }
        self.abort().await;
    }

    /// Cancels the copies at once, for a process that failed to start.
    pub async fn abort(&self) {
        self.cancel.cancel();
        self.outputs.close();
        self.inputs.close();
        tokio::join!(self.outputs.wait(), self.inputs.wait());
    }

    async fn copy<R, W>(cancel: CancellationToken, from: R, to: W)
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (mut src, mut dst) = (from, to);
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("io shut down, copy task exits");
            },
            res = tokio::io::copy(&mut src, &mut dst) => match res {
                Ok(_) => {}
                // A console hanging up (EIO) or a pipe losing its reader
                // (EPIPE) is how a copy normally ends.
                Err(e) if e.kind() == ErrorKind::BrokenPipe || e.raw_os_error() == Some(libc::EIO) => {
                    debug!("copy io ended: {}", e);
                }
                Err(e) => error!("copy io failed {}", e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_null() {
        assert!(Stdio::new("", "", "", false).is_null());
        assert!(Stdio::new("", "", "", true).is_null(), "a tty is not stdio");
        assert!(!Stdio::new("/run/in", "", "", false).is_null());
        assert!(!Stdio::new("", "/run/out", "", false).is_null());
        assert!(!Stdio::new("", "", "/run/err", false).is_null());
    }

    #[cfg(target_os = "linux")]
    mod copies {
        use std::{
            io::Write,
            os::fd::OwnedFd,
            path::{Path, PathBuf},
            time::Instant,
        };

        use nix::sys::stat::Mode;
        use tokio::{
            io::AsyncReadExt,
            process::{Child, Command},
            time::timeout,
        };

        use super::*;

        fn mkfifo(dir: &Path, name: &str) -> PathBuf {
            let path = dir.join(name);
            nix::unistd::mkfifo(&path, Mode::S_IRWXU).unwrap();
            path
        }

        /// Starts `command` with the stdin and stdout `stdio` asks for routed
        /// through the fifos the way a shim does for a container process.
        async fn spawn_copied(stdio: &Stdio, mut command: Command) -> (Child, IoTasks) {
            let stdin = (!stdio.stdin.is_empty()).then(|| std::io::pipe().unwrap());
            let stdout = (!stdio.stdout.is_empty()).then(|| std::io::pipe().unwrap());
            if let Some((rd, _)) = &stdin {
                command.stdin(rd.try_clone().unwrap());
            }
            if let Some((_, wr)) = &stdout {
                command.stdout(wr.try_clone().unwrap());
            }
            let child = command.spawn().unwrap();
            // The command keeps its copies of the child's pipe ends until dropped.
            drop(command);

            let io_tasks = IoTasks::default();
            if let Some((_, wr)) = stdin {
                let to = pipe::Sender::from_owned_fd(OwnedFd::from(wr)).unwrap();
                io_tasks.copy_from_fifo(&stdio.stdin, to).unwrap();
            }
            if let Some((rd, _)) = stdout {
                let from = pipe::Receiver::from_owned_fd(OwnedFd::from(rd)).unwrap();
                io_tasks.copy_to_fifo(from, &stdio.stdout).unwrap();
            }
            (child, io_tasks)
        }

        /// A deleted process's fifos must be released even while a child it
        /// left behind still holds its stdio open.
        #[tokio::test]
        async fn shutdown_releases_fifos_a_child_holds_open() {
            let dir = tempfile::tempdir().unwrap();
            let path = mkfifo(dir.path(), "stdout");
            let stdio = Stdio::new("", path.to_str().unwrap(), "", false);
            // containerd opens its end before the process starts.
            let mut reader = pipe::OpenOptions::new().open_receiver(&path).unwrap();
            let mut command = Command::new("sh");
            command
                .args(["-c", "echo hello; exec sleep 30"])
                .kill_on_drop(true);
            let (_child, io_tasks) = spawn_copied(&stdio, command).await;

            let mut buf = [0u8; 16];
            let n = reader.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"hello\n");
            // sleep keeps the pipe open, so the copy keeps the fifo open too.
            assert!(timeout(Duration::from_millis(200), reader.read(&mut buf))
                .await
                .is_err());

            let drain = Duration::from_millis(100);
            let started = Instant::now();
            io_tasks.shutdown_within(drain).await;
            assert!(started.elapsed() >= drain);
            assert_eq!(reader.read(&mut buf).await.unwrap(), 0, "fifo released");
        }

        /// Shutdown must not wait for stdin, whose fifo containerd commonly
        /// keeps open past delete.
        #[tokio::test]
        async fn shutdown_does_not_drain_stdin() {
            let dir = tempfile::tempdir().unwrap();
            let path = mkfifo(dir.path(), "stdin");
            let stdio = Stdio::new(path.to_str().unwrap(), "", "", false);
            // containerd's write end stays open throughout.
            let _writer = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            let mut command = Command::new("sleep");
            command.arg("30").kill_on_drop(true);
            let (_child, io_tasks) = spawn_copied(&stdio, command).await;

            let drain = Duration::from_secs(1);
            let started = Instant::now();
            io_tasks.shutdown_within(drain).await;
            assert!(started.elapsed() < drain);
            assert!(io_tasks.inputs.is_empty(), "the stdin copy was cancelled");
        }

        /// Setting up stdin must not wait for containerd to attach a writer,
        /// and the process must not see EOF before one has.
        #[tokio::test]
        async fn stdin_waits_for_a_writer() {
            let dir = tempfile::tempdir().unwrap();
            let stdin = mkfifo(dir.path(), "stdin");
            let stdout = mkfifo(dir.path(), "stdout");
            let stdio = Stdio::new(stdin.to_str().unwrap(), stdout.to_str().unwrap(), "", false);
            let mut reader = pipe::OpenOptions::new().open_receiver(&stdout).unwrap();
            let mut command = Command::new("cat");
            command.kill_on_drop(true);
            let (mut child, _io_tasks) = spawn_copied(&stdio, command).await;

            // Nobody has stdin open for writing yet, so cat keeps waiting.
            let mut buf = [0u8; 16];
            assert!(timeout(Duration::from_millis(200), reader.read(&mut buf))
                .await
                .is_err());
            std::fs::OpenOptions::new()
                .write(true)
                .open(&stdin)
                .unwrap()
                .write_all(b"hi\n")
                .unwrap();
            let n = reader.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"hi\n");
            assert_eq!(reader.read(&mut buf).await.unwrap(), 0, "cat exits on EOF");
            assert!(child.wait().await.unwrap().success());
        }

        /// CloseIO must reach the process as EOF on stdin even while containerd
        /// still holds the fifo open.
        #[tokio::test]
        async fn close_input_ends_stdin() {
            let dir = tempfile::tempdir().unwrap();
            let stdin = mkfifo(dir.path(), "stdin");
            let stdout = mkfifo(dir.path(), "stdout");
            let stdio = Stdio::new(stdin.to_str().unwrap(), stdout.to_str().unwrap(), "", false);
            let mut reader = pipe::OpenOptions::new().open_receiver(&stdout).unwrap();
            let _writer = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&stdin)
                .unwrap();
            let mut command = Command::new("cat");
            command.kill_on_drop(true);
            let (mut child, io_tasks) = spawn_copied(&stdio, command).await;

            io_tasks.close_input().await;
            let mut buf = [0u8; 16];
            assert_eq!(reader.read(&mut buf).await.unwrap(), 0, "cat exits on EOF");
            assert!(child.wait().await.unwrap().success());
        }
    }
}
