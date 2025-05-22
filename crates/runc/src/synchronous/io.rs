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

use std::{
    fmt::Debug,
    fs::{File, OpenOptions},
    io::Result,
    os::unix::{fs::OpenOptionsExt, io::AsRawFd},
    process::Stdio,
    sync::Mutex,
};

use nix::unistd::{Gid, Uid};

use super::Io;
use crate::{Command, Pipe, PipedIo};

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

impl PipedIo {
    pub fn new(uid: u32, gid: u32, opts: &IOOption) -> std::io::Result<Self> {
        Ok(Self {
            stdin: if opts.open_stdin {
                Self::create_pipe(uid, gid, true)?
            } else {
                None
            },
            stdout: if opts.open_stdout {
                Self::create_pipe(uid, gid, true)?
            } else {
                None
            },
            stderr: if opts.open_stderr {
                Self::create_pipe(uid, gid, true)?
            } else {
                None
            },
        })
    }

    fn create_pipe(uid: u32, gid: u32, stdin: bool) -> std::io::Result<Option<Pipe>> {
        let pipe = Pipe::new()?;
        let uid = Some(Uid::from_raw(uid));
        let gid = Some(Gid::from_raw(gid));
        if stdin {
            let rd = pipe.rd.try_clone()?;
            nix::unistd::fchown(rd, uid, gid)?;
        } else {
            let wr = pipe.wr.try_clone()?;
            nix::unistd::fchown(wr, uid, gid)?;
        }
        Ok(Some(pipe))
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
    #[test]
    fn test_create_piped_io() {
        use std::io::{Read, Write};

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
