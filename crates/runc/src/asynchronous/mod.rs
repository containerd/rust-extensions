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

mod runc;
use std::{fmt::Debug, io::Result, process::Stdio};

use async_trait::async_trait;
pub use runc::{DefaultExecutor, Spawner};
use tokio::{
    fs::OpenOptions,
    io::{AsyncRead, AsyncWrite},
    net::unix::pipe::{Receiver, Sender},
};

use crate::{
    io::{InheritedStdIo, NullIo, PipedStdIo, FIFO},
    utils::open_fifo_for_reading,
    Command, Pipe, PipedIo,
};

#[async_trait]
pub trait Io: Debug + Send + Sync {
    fn stdin(&self) -> Option<Box<dyn AsyncWrite + Send + Sync + Unpin>> {
        None
    }

    fn stdout(&self) -> Option<Box<dyn AsyncRead + Send + Sync + Unpin>> {
        None
    }

    fn stderr(&self) -> Option<Box<dyn AsyncRead + Send + Sync + Unpin>> {
        None
    }

    /// Set IO for passed command.
    /// Read side of stdin, write side of stdout and write side of stderr should be provided to command.
    async fn set(&self, cmd: &mut Command) -> Result<()>;

    /// Only close write side (should be stdout/err "from" runc process)
    async fn close_after_start(&self);
}

// tokio's pipe types take ownership of the descriptor they wrap, so the
// accessors never hand out the pipe's own fd, which the reader and the pipe
// would both close. The stdin writer is given away outright, so that dropping
// it reaches the child as EOF while the driver is still alive; the read ends
// are duplicated.
#[async_trait]
impl Io for PipedIo {
    fn stdin(&self) -> Option<Box<dyn AsyncWrite + Send + Sync + Unpin>> {
        let fd = self.stdin.as_ref()?.take_wr()?;
        Some(Box::new(Sender::from_owned_fd(fd).ok()?))
    }

    fn stdout(&self) -> Option<Box<dyn AsyncRead + Send + Sync + Unpin>> {
        self.stdout.as_ref().and_then(read_end)
    }

    fn stderr(&self) -> Option<Box<dyn AsyncRead + Send + Sync + Unpin>> {
        self.stderr.as_ref().and_then(read_end)
    }

    // Note that this internally use [`std::fs::File`]'s `try_clone()`.
    // Thus, the files passed to commands will be not closed after command exit.
    async fn set(&self, cmd: &mut Command) -> std::io::Result<()> {
        if let Some(p) = self.stdin.as_ref() {
            let pr = p.rd.try_clone()?;
            cmd.stdin(pr);
        }

        if let Some(p) = self.stdout.as_ref() {
            let pw = p
                .try_clone_wr()
                .ok_or_else(|| std::io::Error::other("write end closed"))?;
            cmd.stdout(pw);
        }

        if let Some(p) = self.stderr.as_ref() {
            let pw = p
                .try_clone_wr()
                .ok_or_else(|| std::io::Error::other("write end closed"))?;
            cmd.stderr(pw);
        }

        Ok(())
    }

    async fn close_after_start(&self) {
        if let Some(p) = self.stdout.as_ref() {
            p.close_wr();
        }

        if let Some(p) = self.stderr.as_ref() {
            p.close_wr();
        }
    }
}

fn read_end(pipe: &Pipe) -> Option<Box<dyn AsyncRead + Send + Sync + Unpin>> {
    let fd = pipe.rd.try_clone().ok()?;
    Some(Box::new(Receiver::from_owned_fd(fd).ok()?))
}

#[async_trait]
impl Io for NullIo {
    async fn set(&self, cmd: &mut Command) -> std::io::Result<()> {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        Ok(())
    }

    async fn close_after_start(&self) {}
}

#[async_trait]
impl Io for InheritedStdIo {
    async fn set(&self, cmd: &mut Command) -> std::io::Result<()> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        Ok(())
    }

    async fn close_after_start(&self) {}
}

#[async_trait]
impl Io for PipedStdIo {
    async fn set(&self, cmd: &mut Command) -> std::io::Result<()> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(())
    }

    async fn close_after_start(&self) {}
}

#[async_trait]
impl Io for FIFO {
    async fn set(&self, cmd: &mut Command) -> Result<()> {
        if let Some(path) = self.stdin.as_ref() {
            cmd.stdin(open_fifo_for_reading(path)?);
        }

        if let Some(path) = self.stdout.as_ref() {
            let stdout = OpenOptions::new().write(true).open(path).await?;
            cmd.stdout(stdout.into_std().await);
        }

        if let Some(path) = self.stderr.as_ref() {
            let stderr = OpenOptions::new().write(true).open(path).await?;
            cmd.stderr(stderr.into_std().await);
        }

        Ok(())
    }

    async fn close_after_start(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piped_io() -> PipedIo {
        let uid = nix::unistd::getuid().as_raw();
        let gid = nix::unistd::getgid().as_raw();
        PipedIo::new(uid, gid, &crate::io::IoOption::default()).unwrap()
    }

    #[tokio::test]
    async fn piped_io_keeps_stdout_and_stderr_apart() {
        use tokio::io::AsyncReadExt;

        let io = piped_io();
        let mut command = Command::new("sh");
        command.args(["-c", "printf out && printf err >&2"]);
        io.set(&mut command).await.unwrap();
        let mut child = command.spawn().unwrap();
        // The command keeps its copies of the child's pipe ends until dropped.
        drop(command);
        io.close_after_start().await;
        let (mut stdout, mut stderr) = (io.stdout().unwrap(), io.stderr().unwrap());
        // The readers own their descriptors and outlive the pipes they came from.
        drop(io);

        let (mut out, mut err) = (String::new(), String::new());
        stdout.read_to_string(&mut out).await.unwrap();
        stderr.read_to_string(&mut err).await.unwrap();
        assert_eq!((out.as_str(), err.as_str()), ("out", "err"));
        assert!(child.wait().await.unwrap().success());
    }

    /// Dropping the stdin writer must reach the child as EOF while the driver
    /// is still alive, as `Runc::run` keeps it until the child exits.
    #[tokio::test]
    async fn piped_io_stdin_closes_with_its_writer() {
        use std::time::Duration;

        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let io = piped_io();
        let mut command = Command::new("cat");
        io.set(&mut command).await.unwrap();
        let mut child = command.spawn().unwrap();
        drop(command);
        io.close_after_start().await;
        let mut stdin = io.stdin().unwrap();
        stdin.write_all(b"hi").await.unwrap();
        drop(stdin);

        let mut out = String::new();
        let mut stdout = io.stdout().unwrap();
        tokio::time::timeout(Duration::from_secs(5), stdout.read_to_string(&mut out))
            .await
            .expect("cat saw EOF on stdin")
            .unwrap();
        assert_eq!(out, "hi");
        assert!(child.wait().await.unwrap().success());
    }

    #[tokio::test]
    async fn null_io_accepts_child_output() {
        let io = NullIo::new().unwrap();
        let mut command = Command::new("sh");
        command.args(["-c", "printf stdout && printf stderr >&2"]);
        io.set(&mut command).await.unwrap();
        let mut child = command.spawn().unwrap();
        io.close_after_start().await;
        assert!(child.wait().await.unwrap().success());
    }
}
