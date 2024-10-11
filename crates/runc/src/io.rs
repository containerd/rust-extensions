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
#[cfg(not(feature = "async"))]
use std::io::{Read, Write};
use std::{
    fmt::Debug,
    fs::{File, OpenOptions},
    io::Result,
    os::unix::{
        fs::OpenOptionsExt,
        io::{AsRawFd, OwnedFd},
    },
    process::Stdio,
    sync::Mutex,
};

use log::debug;
use nix::unistd::{Gid, Uid};
#[cfg(feature = "async")]
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::unix::pipe;

use crate::Command;

pub trait Io: Debug + Send + Sync {
    /// Return write side of stdin
    #[cfg(not(feature = "async"))]
    fn stdin(&self) -> Option<Box<dyn Write + Send + Sync>> {
        None
    }

    /// Return read side of stdout
    #[cfg(not(feature = "async"))]
    fn stdout(&self) -> Option<Box<dyn Read + Send>> {
        None
    }

    /// Return read side of stderr
    #[cfg(not(feature = "async"))]
    fn stderr(&self) -> Option<Box<dyn Read + Send>> {
        None
    }

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

#[derive(Debug, Clone)]
pub struct IOOption {
    pub open_stdin: bool,
    pub open_stdout: bool,
    pub open_stderr: bool,
}

impl Default for IOOption {
    fn default() -> Self {
        Self {
            open_stdin: true,
            open_stdout: true,
            open_stderr: true,
        }
    }
}

/// Struct to represent a pipe that can be used to transfer stdio inputs and outputs.
///
/// With this Io driver, methods of [crate::Runc] may capture the output/error messages.
/// When one side of the pipe is closed, the state will be represented with [`None`].
#[derive(Debug)]
pub struct Pipe {
    rd: OwnedFd,
    wr: OwnedFd,
}

#[derive(Debug)]
pub struct PipedIo {
    stdin: Option<Pipe>,
    stdout: Option<Pipe>,
    stderr: Option<Pipe>,
}

impl Pipe {
    fn new() -> std::io::Result<Self> {
        let (tx, rx) = pipe::pipe()?;
        let rd = tx.into_blocking_fd()?;
        let wr = rx.into_blocking_fd()?;
        Ok(Self { rd, wr })
    }
}

impl PipedIo {
    pub fn new(uid: u32, gid: u32, opts: &IOOption) -> std::io::Result<Self> {
        Ok(Self {
            stdin: Self::create_pipe(uid, gid, opts.open_stdin, true)?,
            stdout: Self::create_pipe(uid, gid, opts.open_stdout, false)?,
            stderr: Self::create_pipe(uid, gid, opts.open_stderr, false)?,
        })
    }

    fn create_pipe(
        uid: u32,
        gid: u32,
        enabled: bool,
        stdin: bool,
    ) -> std::io::Result<Option<Pipe>> {
        if !enabled {
            return Ok(None);
        }

        let pipe = Pipe::new()?;
        let uid = Some(Uid::from_raw(uid));
        let gid = Some(Gid::from_raw(gid));
        if stdin {
            let rd = pipe.rd.try_clone()?;
            nix::unistd::fchown(rd.as_raw_fd(), uid, gid)?;
        } else {
            let wr = pipe.wr.try_clone()?;
            nix::unistd::fchown(wr.as_raw_fd(), uid, gid)?;
        }
        Ok(Some(pipe))
    }
}

impl Io for PipedIo {
    #[cfg(not(feature = "async"))]
    fn stdin(&self) -> Option<Box<dyn Write + Send + Sync>> {
        self.stdin.as_ref().and_then(|pipe| {
            pipe.wr
                .try_clone()
                .map(|x| Box::new(x) as Box<dyn Write + Send + Sync>)
                .ok()
        })
    }

    #[cfg(feature = "async")]
    fn stdin(&self) -> Option<Box<dyn AsyncWrite + Send + Sync + Unpin>> {
        self.stdin.as_ref().and_then(|pipe| {
            let fd = pipe.wr.as_raw_fd();
            tokio_pipe::PipeWrite::from_raw_fd_checked(fd)
                .map(|x| Box::new(x) as Box<dyn AsyncWrite + Send + Sync + Unpin>)
                .ok()
        })
    }

    #[cfg(not(feature = "async"))]
    fn stdout(&self) -> Option<Box<dyn Read + Send>> {
        self.stdout.as_ref().and_then(|pipe| {
            pipe.rd
                .try_clone()
                .map(|x| Box::new(x) as Box<dyn Read + Send>)
                .ok()
        })
    }

    #[cfg(feature = "async")]
    fn stdout(&self) -> Option<Box<dyn AsyncRead + Send + Sync + Unpin>> {
        self.stdout.as_ref().and_then(|pipe| {
            let fd = pipe.rd.as_raw_fd();
            tokio_pipe::PipeRead::from_raw_fd_checked(fd)
                .map(|x| Box::new(x) as Box<dyn AsyncRead + Send + Sync + Unpin>)
                .ok()
        })
    }

    #[cfg(not(feature = "async"))]
    fn stderr(&self) -> Option<Box<dyn Read + Send>> {
        self.stderr.as_ref().and_then(|pipe| {
            pipe.rd
                .try_clone()
                .map(|x| Box::new(x) as Box<dyn Read + Send>)
                .ok()
        })
    }

    #[cfg(feature = "async")]
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

/// IO driver to direct output/error messages to /dev/null.
///
/// With this Io driver, all methods of [crate::Runc] can't capture the output/error messages.
#[derive(Debug)]
pub struct NullIo {
    dev_null: Mutex<Option<File>>,
}

impl NullIo {
    pub fn new() -> std::io::Result<Self> {
        let f = OpenOptions::new().read(true).open("/dev/null")?;
        let dev_null = Mutex::new(Some(f));
        Ok(Self { dev_null })
    }
}

impl Io for NullIo {
    fn set(&self, cmd: &mut Command) -> std::io::Result<()> {
        if let Some(null) = self.dev_null.lock().unwrap().as_ref() {
            cmd.stdout(null.try_clone()?);
            cmd.stderr(null.try_clone()?);
        }
        Ok(())
    }

    fn close_after_start(&self) {
        let mut m = self.dev_null.lock().unwrap();
        let _ = m.take();
    }
}

/// Io driver based on Stdio::inherited(), to direct outputs/errors to stdio.
///
/// With this Io driver, all methods of [crate::Runc] can't capture the output/error messages.
#[derive(Debug)]
pub struct InheritedStdIo {}

impl InheritedStdIo {
    pub fn new() -> std::io::Result<Self> {
        Ok(InheritedStdIo {})
    }
}

impl Io for InheritedStdIo {
    fn set(&self, cmd: &mut Command) -> std::io::Result<()> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        Ok(())
    }

    fn close_after_start(&self) {}
}

/// Io driver based on Stdio::piped(), to capture outputs/errors from runC.
///
/// With this Io driver, methods of [crate::Runc] may capture the output/error messages.
#[derive(Debug)]
pub struct PipedStdIo {}

impl PipedStdIo {
    pub fn new() -> std::io::Result<Self> {
        Ok(PipedStdIo {})
    }
}

impl Io for PipedStdIo {
    fn set(&self, cmd: &mut Command) -> std::io::Result<()> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(())
    }

    fn close_after_start(&self) {}
}

/// FIFO for the scenario that set FIFO for command Io.
#[derive(Debug)]
pub struct FIFO {
    pub stdin: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

impl Io for FIFO {
    fn set(&self, cmd: &mut Command) -> Result<()> {
        if let Some(path) = self.stdin.as_ref() {
            let stdin = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)?;
            cmd.stdin(stdin);
        }

        if let Some(path) = self.stdout.as_ref() {
            let stdout = OpenOptions::new().write(true).open(path)?;
            cmd.stdout(stdout);
        }

        if let Some(path) = self.stderr.as_ref() {
            let stderr = OpenOptions::new().write(true).open(path)?;
            cmd.stderr(stderr);
        }

        Ok(())
    }

    fn close_after_start(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_io_option() {
        let opts = IOOption {
            open_stdin: false,
            open_stdout: false,
            open_stderr: false,
        };
        let io = PipedIo::new(1000, 1000, &opts).unwrap();

        assert!(io.stdin().is_none());
        assert!(io.stdout().is_none());
        assert!(io.stderr().is_none());
    }

    #[cfg(target_os = "linux")]
    #[cfg(not(feature = "async"))]
    #[test]
    fn test_create_piped_io() {
        let opts = IOOption::default();
        let uid = nix::unistd::getuid();
        let gid = nix::unistd::getgid();
        let io = PipedIo::new(uid.as_raw(), gid.as_raw(), &opts).unwrap();
        let mut buf = [0xfau8];

        let mut stdin = io.stdin().unwrap();
        stdin.write_all(&buf).unwrap();
        buf[0] = 0x0;

        io.stdin
            .as_ref()
            .map(|v| v.rd.try_clone().unwrap().read(&mut buf).unwrap());
        assert_eq!(&buf, &[0xfau8]);

        let mut stdout = io.stdout().unwrap();
        buf[0] = 0xce;
        io.stdout
            .as_ref()
            .map(|v| v.wr.try_clone().unwrap().write(&buf).unwrap());
        buf[0] = 0x0;
        stdout.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, &[0xceu8]);

        let mut stderr = io.stderr().unwrap();
        buf[0] = 0xa5;
        io.stderr
            .as_ref()
            .map(|v| v.wr.try_clone().unwrap().write(&buf).unwrap());
        buf[0] = 0x0;
        stderr.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, &[0xa5u8]);

        io.close_after_start();
        stdout.read_exact(&mut buf).unwrap_err();
        stderr.read_exact(&mut buf).unwrap_err();
    }

    #[test]
    fn test_null_io() {
        let io = NullIo::new().unwrap();
        assert!(io.stdin().is_none());
        assert!(io.stdout().is_none());
        assert!(io.stderr().is_none());
    }
}
