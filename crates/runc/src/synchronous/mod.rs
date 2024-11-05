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
use std::{
    fmt::Debug,
    io::{Read, Result, Write},
    os::fd::AsRawFd,
};

use log::debug;
pub use pipe::Pipe;

use crate::Command;

pub trait Io: Debug + Send + Sync {
    /// Return write side of stdin
    fn stdin(&self) -> Option<Box<dyn Write + Send + Sync>> {
        None
    }

    /// Return read side of stdout
    fn stdout(&self) -> Option<Box<dyn Read + Send>> {
        None
    }

    /// Return read side of stderr
    fn stderr(&self) -> Option<Box<dyn Read + Send>> {
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
    fn stdin(&self) -> Option<Box<dyn Write + Send + Sync>> {
        self.stdin.as_ref().and_then(|pipe| {
            pipe.wr
                .try_clone()
                .map(|x| Box::new(x) as Box<dyn Write + Send + Sync>)
                .ok()
        })
    }

    fn stdout(&self) -> Option<Box<dyn Read + Send>> {
        self.stdout.as_ref().and_then(|pipe| {
            pipe.rd
                .try_clone()
                .map(|x| Box::new(x) as Box<dyn Read + Send>)
                .ok()
        })
    }

    fn stderr(&self) -> Option<Box<dyn Read + Send>> {
        self.stderr.as_ref().and_then(|pipe| {
            pipe.rd
                .try_clone()
                .map(|x| Box::new(x) as Box<dyn Read + Send>)
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
