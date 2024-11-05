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

mod pipe;
use std::{fmt::Debug, io::Result, os::fd::AsRawFd};

use log::debug;
pub use pipe::Pipe;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::Command;

pub trait Io: Debug + Send + Sync {
    /// Return write side of stdin
    #[cfg(feature = "async")]
    fn stdin(&self) -> Option<Box<dyn AsyncWrite + Send + Sync + Unpin>> {
        None
    }

    /// Return read side of stdout
    #[cfg(feature = "async")]
    fn stdout(&self) -> Option<Box<dyn AsyncRead + Send + Sync + Unpin>> {
        None
    }

    /// Return read side of stderr
    #[cfg(feature = "async")]
    fn stderr(&self) -> Option<Box<dyn AsyncRead + Send + Sync + Unpin>> {
        None
    }

    /// Set IO for passed command.
    /// Read side of stdin, write side of stdout and write side of stderr should be provided to command.
    fn set(&self, cmd: &mut Command) -> Result<()>;

    /// Only close write side (should be stdout/err "from" runc process)
    fn close_after_start(&self);
}

#[derive(Debug)]
pub struct PipedIo {
    pub stdin: Option<Pipe>,
    pub stdout: Option<Pipe>,
    pub stderr: Option<Pipe>,
}

impl Io for PipedIo {
    fn stdin(&self) -> Option<Box<dyn AsyncWrite + Send + Sync + Unpin>> {
        self.stdin.as_ref().and_then(|pipe| {
            let fd = pipe.wr.as_raw_fd();
            tokio_pipe::PipeWrite::from_raw_fd_checked(fd)
                .map(|x| Box::new(x) as Box<dyn AsyncWrite + Send + Sync + Unpin>)
                .ok()
        })
    }

    fn stdout(&self) -> Option<Box<dyn AsyncRead + Send + Sync + Unpin>> {
        self.stdout.as_ref().and_then(|pipe| {
            let fd = pipe.rd.as_raw_fd();
            tokio_pipe::PipeRead::from_raw_fd_checked(fd)
                .map(|x| Box::new(x) as Box<dyn AsyncRead + Send + Sync + Unpin>)
                .ok()
        })
    }

    fn stderr(&self) -> Option<Box<dyn AsyncRead + Send + Sync + Unpin>> {
        self.stderr.as_ref().and_then(|pipe| {
            let fd = pipe.rd.as_raw_fd();
            tokio_pipe::PipeRead::from_raw_fd_checked(fd)
                .map(|x| Box::new(x) as Box<dyn AsyncRead + Send + Sync + Unpin>)
                .ok()
        })
    }

    // Note that this internally use [`std::fs::File`]'s `try_clone()`.
    // Thus, the files passed to commands will be not closed after command exit.
    fn set(&self, cmd: &mut Command) -> std::io::Result<()> {
        if let Some(p) = self.stdin.as_ref() {
            let pr = p.rd.try_clone()?;
            cmd.stdin(pr);
        }

        if let Some(p) = self.stdout.as_ref() {
            let pw = p.wr.try_clone()?;
            cmd.stdout(pw);
        }

        if let Some(p) = self.stderr.as_ref() {
            let pw = p.wr.try_clone()?;
            cmd.stdout(pw);
        }

        Ok(())
    }

    fn close_after_start(&self) {
        if let Some(p) = self.stdout.as_ref() {
            nix::unistd::close(p.wr.as_raw_fd()).unwrap_or_else(|e| debug!("close stdout: {}", e));
        }

        if let Some(p) = self.stderr.as_ref() {
            nix::unistd::close(p.wr.as_raw_fd()).unwrap_or_else(|e| debug!("close stderr: {}", e));
        }
    }
}
