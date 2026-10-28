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

use std::{
    fs::{File, OpenOptions},
    io::{ErrorKind, Read, Write},
    os::{
        fd::{AsFd, BorrowedFd},
        unix::fs::OpenOptionsExt,
    },
    pin::Pin,
    sync::{Arc, Mutex},
    task::{ready, Context, Poll},
    time::Duration,
};

use log::{debug, error, warn};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use tokio::{
    io::{unix::AsyncFd, AsyncRead, AsyncWrite, ReadBuf},
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
#[derive(Clone, Default)]
pub struct IoTasks {
    outputs: TaskTracker,
    inputs: TaskTracker,
    cancel: CancellationToken,
    /// The shim's hold on the stdin fifo, see [`IoTasks::copy_from_fifo`].
    stdin: Arc<Mutex<Hold>>,
}

/// The shim's own writer on a process's stdin fifo.
#[derive(Default)]
enum Hold {
    /// No copy has started yet.
    #[default]
    Pending,
    /// Kept only to be dropped, never read.
    Open(#[allow(dead_code)] File),
    /// CloseIO let go of it, or came before there was anything to hold.
    Released,
}

impl IoTasks {
    /// Copies the process's input from the fifo at `path` `to` the runtime.
    ///
    /// Neither open waits for containerd, and the shim holds the fifo open for
    /// writing itself from before its reader opens, as the Go shim does, so
    /// that containerd's writers coming and going never end the copy: it
    /// ends, and with it the pipe into the process, only once CloseIO has let
    /// go of that hold and containerd's writer is gone, having delivered
    /// whatever was written first. A CloseIO that came before the copy counts
    /// as well: the hold is then dropped as soon as it is taken, which is also
    /// what makes a fifo nobody has written to report EOF. Shutdown cancels
    /// the copy without draining it, as containerd commonly keeps a stdin
    /// fifo open past delete.
    pub fn copy_from_fifo<W>(&self, path: &str, to: W) -> Result<()>
    where
        W: AsyncWrite + Send + Unpin + 'static,
    {
        // Opened for reading as well, so that it cannot fail for want of a
        // reader; it bridges the time until the hold proper is in place.
        let bridge = hold_fifo(path, OpenOptions::new().read(true).write(true))?;
        let fifo = pipe::OpenOptions::new()
            .open_receiver(path)
            .map_err(io_error!(e, "open {}", path))?;
        let hold = hold_fifo(path, OpenOptions::new().write(true))?;
        drop(bridge);
        let mut stdin = self.stdin.lock().unwrap();
        match *stdin {
            Hold::Released => drop(hold),
            _ => *stdin = Hold::Open(hold),
        }
        drop(stdin);
        self.inputs.spawn(Self::copy(self.cancel.clone(), fifo, to));
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
        let hold = hold_fifo(path, OpenOptions::new().read(true))?;
        // Driven by the reactor rather than the blocking pool: a parked write
        // costs no thread, and dropping the writer releases the fifo at once.
        let fifo = pipe::OpenOptions::new()
            .open_sender(path)
            .map_err(io_error!(e, "open {}", path))?;
        let copy = Self::copy(self.cancel.clone(), from, fifo);
        self.outputs.spawn(async move {
            let _hold = hold;
            copy.await;
        });
        Ok(())
    }

    /// Lets go of the shim's hold on stdin, as CloseIO asks; what the process
    /// sees then is on [`IoTasks::copy_from_fifo`].
    pub fn close_input(&self) {
        *self.stdin.lock().unwrap() = Hold::Released;
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

    /// Fresh tasks to start the process again with after a failed start,
    /// which still count a CloseIO that came already.
    pub fn renew(&self) -> Self {
        let stdin = match *self.stdin.lock().unwrap() {
            Hold::Released => Hold::Released,
            _ => Hold::Pending,
        };
        Self {
            stdin: Arc::new(Mutex::new(stdin)),
            ..Default::default()
        }
    }

    /// Cancels the copies and lets go of stdin at once: the end of every
    /// shutdown, and all of it for a process that failed to start.
    pub async fn abort(&self) {
        self.cancel.cancel();
        self.close_input();
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

/// Opens the fifo at `path` as `options` say, for the shim to hold it open
/// without waiting for the other end.
fn hold_fifo(path: &str, options: &mut OpenOptions) -> Result<File> {
    options
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(io_error!(e, "hold {} open", path))
}

/// The master side of a process's terminal, shared by its copies and its
/// resize handle so that the shim holds the terminal once, and driven by the
/// reactor rather than the blocking pool: a copy parks no thread, and
/// cancelling it ends it at once instead of waiting for the terminal to hang
/// up.
#[derive(Clone)]
pub struct Pty(Arc<AsyncFd<File>>);

impl Pty {
    pub fn new(master: File) -> Result<Self> {
        set_nonblocking(&master).map_err(io_error!(e, "set the console non-blocking"))?;
        // SAFETY: the AsyncFd owns the file, so its descriptor stays open and
        // the same for as long as the registration lives.
        let master = unsafe { AsyncFd::register(master) }
            .map_err(std::io::Error::from)
            .map_err(io_error!(e, "register the console"))?;
        Ok(Self(Arc::new(master)))
    }
}

impl AsFd for Pty {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.get_ref().as_fd()
    }
}

impl AsyncRead for Pty {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            let mut guard = ready!(self.0.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            match guard.try_io(|master| {
                let mut file = master.get_ref();
                file.read(unfilled)
            }) {
                Ok(read) => return Poll::Ready(read.map(|n| buf.advance(n))),
                Err(_would_block) => continue,
            }
        }
    }
}

impl AsyncWrite for Pty {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        loop {
            let mut guard = ready!(self.0.poll_write_ready(cx))?;
            match guard.try_io(|master| {
                let mut file = master.get_ref();
                file.write(buf)
            }) {
                Ok(written) => return Poll::Ready(written),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn set_nonblocking(file: &File) -> std::io::Result<()> {
    let flags = OFlag::from_bits_retain(fcntl(file, FcntlArg::F_GETFL)?);
    if !flags.contains(OFlag::O_NONBLOCK) {
        fcntl(file, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    }
    Ok(())
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
            io::{Read, Write},
            os::fd::OwnedFd,
            path::{Path, PathBuf},
            time::Instant,
        };

        use nix::sys::{
            stat::Mode,
            termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg},
        };
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

        /// What this process's descriptors refer to.
        fn fd_targets() -> Vec<PathBuf> {
            std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(|entry| std::fs::read_link(entry.unwrap().path()).ok())
                .collect()
        }

        /// How many of this process's descriptors refer to a terminal master,
        /// `/dev/ptmx` or `/dev/pts/ptmx` depending on the devpts layout.
        fn ptmx_fds() -> usize {
            fd_targets()
                .iter()
                .filter(|target| target.file_name().is_some_and(|name| name == "ptmx"))
                .count()
        }

        /// Starts `command` with the stdin and stdout `stdio` asks for routed
        /// through the fifos by `io_tasks`, the way a shim does for a container
        /// process.
        async fn spawn_copied(stdio: &Stdio, mut command: Command, io_tasks: &IoTasks) -> Child {
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

            if let Some((_, wr)) = stdin {
                let to = pipe::Sender::from_owned_fd(OwnedFd::from(wr)).unwrap();
                io_tasks.copy_from_fifo(&stdio.stdin, to).unwrap();
            }
            if let Some((rd, _)) = stdout {
                let from = pipe::Receiver::from_owned_fd(OwnedFd::from(rd)).unwrap();
                io_tasks.copy_to_fifo(from, &stdio.stdout).unwrap();
            }
            child
        }

        /// A `cat` with its stdin and stdout routed through fifos by
        /// `io_tasks`, and containerd's end of its stdout.
        struct Cat {
            stdin: PathBuf,
            stdout: pipe::Receiver,
            child: Child,
            io_tasks: IoTasks,
        }

        async fn spawn_cat(dir: &Path, io_tasks: IoTasks) -> Cat {
            let stdin = mkfifo(dir, "stdin");
            let stdout = mkfifo(dir, "stdout");
            let stdio = Stdio::new(stdin.to_str().unwrap(), stdout.to_str().unwrap(), "", false);
            // containerd opens its end before the process starts.
            let stdout = pipe::OpenOptions::new().open_receiver(&stdout).unwrap();
            let mut command = Command::new("cat");
            command.kill_on_drop(true);
            let child = spawn_copied(&stdio, command, &io_tasks).await;
            Cat {
                stdin,
                stdout,
                child,
                io_tasks,
            }
        }

        /// containerd's end of a stdin fifo.
        fn writer(path: &Path) -> File {
            OpenOptions::new().write(true).open(path).unwrap()
        }

        /// Asserts that nothing arrives on `stdout` for a while.
        async fn assert_no_output(stdout: &mut pipe::Receiver) {
            let mut buf = [0u8; 16];
            assert!(timeout(Duration::from_millis(200), stdout.read(&mut buf))
                .await
                .is_err());
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
            let io_tasks = IoTasks::default();
            let _child = spawn_copied(&stdio, command, &io_tasks).await;

            let mut buf = [0u8; 16];
            let n = reader.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"hello\n");
            // sleep keeps the pipe open, so the copy keeps the fifo open too.
            assert_no_output(&mut reader).await;

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
            let io_tasks = IoTasks::default();
            let _child = spawn_copied(&stdio, command, &io_tasks).await;

            let drain = Duration::from_secs(1);
            let started = Instant::now();
            io_tasks.shutdown_within(drain).await;
            assert!(started.elapsed() < drain);
            assert!(io_tasks.inputs.is_empty(), "the stdin copy was cancelled");
        }

        /// Setting up stdin must not wait for containerd to attach a writer,
        /// and the process must not see EOF before CloseIO, however often
        /// containerd's writers come and go.
        #[tokio::test]
        async fn stdin_waits_for_close_io() {
            let dir = tempfile::tempdir().unwrap();
            let mut cat = spawn_cat(dir.path(), IoTasks::default()).await;

            // Nobody has stdin open for writing yet, so cat keeps waiting.
            assert_no_output(&mut cat.stdout).await;
            let mut buf = [0u8; 16];
            for line in [&b"hi\n"[..], b"again\n"] {
                writer(&cat.stdin).write_all(line).unwrap();
                let n = cat.stdout.read(&mut buf).await.unwrap();
                assert_eq!(&buf[..n], line);
            }
            // The writer is gone, but containerd has not asked to close stdin.
            assert_no_output(&mut cat.stdout).await;
            cat.io_tasks.close_input();
            assert_eq!(
                cat.stdout.read(&mut buf).await.unwrap(),
                0,
                "cat exits on EOF"
            );
            assert!(cat.child.wait().await.unwrap().success());
        }

        /// Console copies work both ways through one descriptor, and with the
        /// terminal still open, shutting the runtime down waits on no thread
        /// parked on it: the copies ran on the reactor.
        #[test]
        fn pty_copies_run_on_the_reactor() {
            let dir = tempfile::tempdir().unwrap();
            let stdin = mkfifo(dir.path(), "stdin");
            let stdout = mkfifo(dir.path(), "stdout");
            let pty = nix::pty::openpty(None, None).unwrap();
            // Raw mode: no echo, no line buffering, no newline translation.
            let mut termios = tcgetattr(&pty.slave).unwrap();
            cfmakeraw(&mut termios);
            tcsetattr(&pty.slave, SetArg::TCSANOW, &termios).unwrap();
            let mut slave = File::from(pty.slave);
            let master = File::from(pty.master);

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let io_tasks = IoTasks::default();
            rt.block_on(async {
                let mut reader = pipe::OpenOptions::new().open_receiver(&stdout).unwrap();
                let pty = Pty::new(master).unwrap();
                io_tasks
                    .copy_to_fifo(pty.clone(), stdout.to_str().unwrap())
                    .unwrap();
                io_tasks
                    .copy_from_fifo(stdin.to_str().unwrap(), pty.clone())
                    .unwrap();
                assert_eq!(
                    ptmx_fds(),
                    1,
                    "the copies share the handle's descriptor: {:?}",
                    fd_targets()
                );

                slave.write_all(b"out").unwrap();
                let mut buf = [0u8; 16];
                let n = timeout(Duration::from_secs(5), reader.read(&mut buf))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(&buf[..n], b"out");

                writer(&stdin).write_all(b"in").unwrap();
                let mut slave_reader = slave.try_clone().unwrap();
                let read = tokio::task::spawn_blocking(move || {
                    let mut buf = [0u8; 16];
                    let n = slave_reader.read(&mut buf).unwrap();
                    buf[..n].to_vec()
                });
                let got = timeout(Duration::from_secs(5), read)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(got, b"in");

                timeout(Duration::from_secs(5), io_tasks.abort())
                    .await
                    .unwrap();
            });
            assert_eq!(
                ptmx_fds(),
                0,
                "the terminal is released with its handles: {:?}",
                fd_targets()
            );

            // The slave is still open, so a read parked on the master would
            // keep the blocking pool, and this shutdown, waiting.
            let started = Instant::now();
            rt.shutdown_timeout(Duration::from_secs(2));
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "a thread stayed parked on the terminal"
            );
            drop(slave);
        }

        /// CloseIO sent right after containerd's last write, the way its
        /// client does, must still deliver that write before the process sees
        /// EOF.
        #[tokio::test(flavor = "current_thread")]
        async fn close_input_delivers_what_was_written() {
            let dir = tempfile::tempdir().unwrap();
            let mut cat = spawn_cat(dir.path(), IoTasks::default()).await;

            // No await between the write and CloseIO, and only this thread
            // runs tasks, so the copy has not had a chance to run in between.
            writer(&cat.stdin).write_all(b"hello\n").unwrap();
            cat.io_tasks.close_input();

            let mut output = Vec::new();
            cat.stdout.read_to_end(&mut output).await.unwrap();
            assert_eq!(output, b"hello\n");
            assert!(cat.child.wait().await.unwrap().success());
        }

        /// After CloseIO the copy carries on until containerd's writer is gone
        /// too, as the Go shim's does.
        #[tokio::test]
        async fn close_input_waits_for_containerd() {
            let dir = tempfile::tempdir().unwrap();
            let mut cat = spawn_cat(dir.path(), IoTasks::default()).await;

            let mut late = writer(&cat.stdin);
            cat.io_tasks.close_input();
            late.write_all(b"late\n").unwrap();
            let mut buf = [0u8; 16];
            let n = cat.stdout.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"late\n");
            assert_no_output(&mut cat.stdout).await;
            drop(late);
            assert_eq!(
                cat.stdout.read(&mut buf).await.unwrap(),
                0,
                "cat exits on EOF"
            );
            assert!(cat.child.wait().await.unwrap().success());
        }

        /// CloseIO can come before the copy starts, between an exec and its
        /// start; it must not be forgotten: the process sees EOF at once, even
        /// though containerd never connects a writer.
        #[tokio::test]
        async fn close_input_before_the_copy_ends_stdin() {
            let dir = tempfile::tempdir().unwrap();
            let io_tasks = IoTasks::default();
            io_tasks.close_input();
            let mut cat = spawn_cat(dir.path(), io_tasks).await;

            let mut output = Vec::new();
            timeout(Duration::from_secs(5), cat.stdout.read_to_end(&mut output))
                .await
                .expect("stdin is not held open")
                .unwrap();
            assert!(output.is_empty());
            assert!(cat.child.wait().await.unwrap().success());
        }

        /// A process started again after a failed start gets fresh tasks that
        /// still count the CloseIO that came before.
        #[tokio::test]
        async fn renewed_tasks_keep_an_early_close_input() {
            let dir = tempfile::tempdir().unwrap();
            let io_tasks = IoTasks::default();
            io_tasks.close_input();
            let renewed = io_tasks.renew();
            io_tasks.abort().await;
            let mut cat = spawn_cat(dir.path(), renewed).await;

            let mut output = Vec::new();
            timeout(Duration::from_secs(5), cat.stdout.read_to_end(&mut output))
                .await
                .expect("stdin is not held open")
                .unwrap();
            assert!(output.is_empty());
            assert!(cat.child.wait().await.unwrap().success());
        }
    }
}
