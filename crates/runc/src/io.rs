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
use std::fmt::{self, Debug, Formatter};
use std::fs::File;
use std::io::Result;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::sync::Mutex;

use nix::unistd::{Gid, Uid};

use crate::Command;

pub trait Io: Sync + Send {
    /// Return write side of stdin
    fn stdin(&self) -> Option<File> {
        None
    }

    /// Return read side of stdout
    fn stdout(&self) -> Option<File> {
        None
    }

    /// Return read side of stderr
    fn stderr(&self) -> Option<File> {
        None
    }

    /// Set IO for passed command.
    /// Read side of stdin, write side of stdout and write side of stderr should be provided to command.
    fn set(&self, cmd: &mut Command) -> Result<()>;

    /// Only close write side (should be stdout/err "from" runc process)
    fn close_after_start(&self);
}

impl Debug for dyn Io {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Io",)
    }
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
/// When one side of the pipe is closed, the state will be represented with [`None`].
#[derive(Debug)]
pub struct Pipe {
    // Might be ugly hack: using mutex in order to take rd/wr under immutable [`Pipe`]
    rd: Mutex<Option<File>>,
    wr: Mutex<Option<File>>,
}

impl Pipe {
    pub fn new() -> std::io::Result<Self> {
        let (r, w) = nix::unistd::pipe()?;
        let (rd, wr) = unsafe {
            (
                Mutex::new(Some(File::from_raw_fd(r))),
                Mutex::new(Some(File::from_raw_fd(w))),
            )
        };
        Ok(Self { rd, wr })
    }

    pub fn take_read(&self) -> Option<File> {
        let mut m = self.rd.lock().unwrap();
        m.take()
    }

    pub fn take_write(&self) -> Option<File> {
        let mut m = self.wr.lock().unwrap();
        m.take()
    }

    pub fn close_read(&self) {
        let mut m = self.rd.lock().unwrap();
        let _ = m.take();
    }

    pub fn close_write(&self) {
        let mut m = self.wr.lock().unwrap();
        let _ = m.take();
    }
}

#[derive(Debug)]
pub struct PipedIo {
    stdin: Option<Pipe>,
    stdout: Option<Pipe>,
    stderr: Option<Pipe>,
}

impl PipedIo {
    pub fn new(uid: u32, gid: u32, opts: IOOption) -> std::io::Result<Self> {
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
        let guard = if stdin {
            pipe.rd.lock().unwrap()
        } else {
            pipe.wr.lock().unwrap()
        };
        if let Some(f) = guard.as_ref() {
            let uid = Some(Uid::from_raw(uid));
            let gid = Some(Gid::from_raw(gid));
            nix::unistd::fchown(f.as_raw_fd(), uid, gid)?;
        }
        drop(guard);

        Ok(Some(pipe))
    }
}

impl Io for PipedIo {
    fn stdin(&self) -> Option<File> {
        self.stdin.as_ref().map(|v| v.take_write()).flatten()
    }

    fn stdout(&self) -> Option<File> {
        self.stdout.as_ref().map(|v| v.take_read()).flatten()
    }

    fn stderr(&self) -> Option<File> {
        self.stderr.as_ref().map(|v| v.take_read()).flatten()
    }

    // Note that this internally use [`std::fs::File`]'s `try_clone()`.
    // Thus, the files passed to commands will be not closed after command exit.
    fn set(&self, cmd: &mut Command) -> std::io::Result<()> {
        if let Some(ref p) = self.stdin {
            let m = p.rd.lock().unwrap();
            if let Some(stdin) = &*m {
                let f = stdin.try_clone()?;
                cmd.stdin(f);
            }
        }

        if let Some(ref p) = self.stdout {
            let m = p.wr.lock().unwrap();
            if let Some(f) = &*m {
                let f = f.try_clone()?;
                cmd.stdout(f);
            }
        }

        if let Some(ref p) = self.stderr {
            let m = p.wr.lock().unwrap();
            if let Some(f) = &*m {
                let f = f.try_clone()?;
                cmd.stderr(f);
            }
        }

        Ok(())
    }

    fn close_after_start(&self) {
        if let Some(ref p) = self.stdout {
            p.close_write();
        }
        if let Some(ref p) = self.stderr {
            p.close_write();
        }
    }
}

// IO setup for /dev/null use with runc
#[derive(Debug)]
pub struct NullIo {
    dev_null: Mutex<Option<File>>,
}

impl NullIo {
    pub fn new() -> std::io::Result<Self> {
        let fd = nix::fcntl::open(
            "/dev/null",
            nix::fcntl::OFlag::O_RDONLY,
            nix::sys::stat::Mode::empty(),
        )?;
        let dev_null = unsafe { Mutex::new(Some(std::fs::File::from_raw_fd(fd))) };
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
