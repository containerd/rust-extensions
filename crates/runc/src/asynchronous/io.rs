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

use std::{fmt::Debug, io::Result, process::Stdio};

use async_trait::async_trait;
use nix::unistd::{Gid, Uid};
use tokio::fs::OpenOptions;

pub use crate::Io;
use crate::{utils::open_fifo_for_reading, Command, Pipe, PipedIo};

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
            let wr = pipe
                .try_clone_wr()
                .ok_or_else(|| std::io::Error::other("write end closed"))?;
            nix::unistd::fchown(wr, uid, gid)?;
        }
        Ok(Some(pipe))
    }
}

/// IO driver to direct output/error messages to /dev/null.
///
/// With this Io driver, all methods of [crate::Runc] can't capture the output/error messages.
#[derive(Debug)]
pub struct NullIo {}

impl NullIo {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {})
    }
}

#[async_trait]
impl Io for NullIo {
    async fn set(&self, cmd: &mut Command) -> std::io::Result<()> {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        Ok(())
    }

    async fn close_after_start(&self) {}
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

/// FIFO for the scenario that set FIFO for command Io.
#[derive(Debug)]
pub struct FIFO {
    pub stdin: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
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

    #[tokio::test]
    async fn test_null_io() {
        let io = NullIo::new().unwrap();
        assert!(io.stdin().is_none());
        assert!(io.stdout().is_none());
        assert!(io.stderr().is_none());
    }
}
