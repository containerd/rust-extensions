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
pub mod io;
mod pipe;
mod runc;
use std::{fmt::Debug, io::Result};

use async_trait::async_trait;
pub use pipe::Pipe;
pub use runc::{DefaultExecutor, Spawner};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::unix::pipe::{Receiver, Sender},
};

use crate::Command;
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

#[derive(Debug)]
pub struct PipedIo {
    pub stdin: Option<Pipe>,
    pub stdout: Option<Pipe>,
    pub stderr: Option<Pipe>,
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
