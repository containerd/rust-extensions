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

//! The stdio drivers a process can be given. [`Io`], which wires a driver
//! into a command, is synchronous or asynchronous with the crate.

#[cfg(target_os = "linux")]
use nix::unistd::{Gid, Uid};

pub use crate::Io;
use crate::Pipe;

#[derive(Debug, Clone)]
pub struct IoOption {
    pub open_stdin: bool,
    pub open_stdout: bool,
    pub open_stderr: bool,
}

impl Default for IoOption {
    fn default() -> Self {
        Self {
            open_stdin: true,
            open_stdout: true,
            open_stderr: true,
        }
    }
}

/// Pipes for a process's stdin, stdout and stderr, owned by the io's user.
#[derive(Debug)]
pub struct PipedIo {
    pub stdin: Option<Pipe>,
    pub stdout: Option<Pipe>,
    pub stderr: Option<Pipe>,
}

impl PipedIo {
    pub fn new(uid: u32, gid: u32, opts: &IoOption) -> std::io::Result<Self> {
        #[cfg(not(target_os = "linux"))]
        let _ = (uid, gid);
        let pipe = || -> std::io::Result<Pipe> {
            let pipe = Pipe::new()?;
            // The pipe belongs to the io's user, as in go-runc, which skips
            // this off Linux as well. Both ends share the pipe, so chowning
            // one covers the other.
            #[cfg(target_os = "linux")]
            nix::unistd::fchown(&pipe.rd, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))?;
            Ok(pipe)
        };
        Ok(Self {
            stdin: opts.open_stdin.then(pipe).transpose()?,
            stdout: opts.open_stdout.then(pipe).transpose()?,
            stderr: opts.open_stderr.then(pipe).transpose()?,
        })
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

/// FIFO for the scenario that set FIFO for command Io.
#[derive(Debug)]
pub struct FIFO {
    pub stdin: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_option_leaves_out_what_is_not_wanted() {
        let opts = IoOption {
            open_stdin: false,
            open_stdout: false,
            open_stderr: false,
        };
        let io = PipedIo::new(1000, 1000, &opts).unwrap();

        assert!(io.stdin().is_none());
        assert!(io.stdout().is_none());
        assert!(io.stderr().is_none());
    }

    #[test]
    fn null_io_has_no_streams() {
        let io = NullIo::new().unwrap();
        assert!(io.stdin().is_none());
        assert!(io.stdout().is_none());
        assert!(io.stderr().is_none());
    }
}
