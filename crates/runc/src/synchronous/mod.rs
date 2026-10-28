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
use std::{
    fmt::Debug,
    fs::{File, OpenOptions},
    io::{Read, Result, Write},
    process::Stdio,
};

pub use runc::{DefaultExecutor, Spawner};

use crate::{
    io::{InheritedStdIo, NullIo, PipedStdIo, FIFO},
    utils::open_fifo_for_reading,
    Command, Pipe, PipedIo,
};

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

// The stdin writer is given away outright, so that dropping it reaches the
// child as EOF while the driver is still alive; the read ends are duplicated.
impl Io for PipedIo {
    fn stdin(&self) -> Option<Box<dyn Write + Send + Sync>> {
        Some(Box::new(File::from(self.stdin.as_ref()?.take_wr()?)))
    }

    fn stdout(&self) -> Option<Box<dyn Read + Send>> {
        self.stdout.as_ref().and_then(read_end)
    }

    fn stderr(&self) -> Option<Box<dyn Read + Send>> {
        self.stderr.as_ref().and_then(read_end)
    }

    // Note that this internally use [`std::fs::File`]'s `try_clone()`.
    // Thus, the files passed to commands will be not closed after command exit.
    fn set(&self, cmd: &mut Command) -> std::io::Result<()> {
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

    fn close_after_start(&self) {
        if let Some(p) = self.stdout.as_ref() {
            p.close_wr();
        }

        if let Some(p) = self.stderr.as_ref() {
            p.close_wr();
        }
    }
}

fn read_end(pipe: &Pipe) -> Option<Box<dyn Read + Send>> {
    Some(Box::new(File::from(pipe.rd.try_clone().ok()?)))
}

impl Io for NullIo {
    fn set(&self, cmd: &mut Command) -> std::io::Result<()> {
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        Ok(())
    }

    fn close_after_start(&self) {}
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

impl Io for PipedStdIo {
    fn set(&self, cmd: &mut Command) -> std::io::Result<()> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(())
    }

    fn close_after_start(&self) {}
}

impl Io for FIFO {
    fn set(&self, cmd: &mut Command) -> Result<()> {
        if let Some(path) = self.stdin.as_ref() {
            cmd.stdin(open_fifo_for_reading(path)?);
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

    #[test]
    fn test_create_piped_io() {
        let opts = crate::io::IoOption::default();
        let uid = nix::unistd::getuid();
        let gid = nix::unistd::getgid();
        let io = PipedIo::new(uid.as_raw(), gid.as_raw(), &opts).unwrap();
        let mut buf = [0xfau8];

        let mut stdin = io.stdin().unwrap();
        stdin.write_all(&buf).unwrap();
        buf[0] = 0x0;

        io.stdin.as_ref().map(|v| {
            File::from(v.rd.try_clone().unwrap())
                .read(&mut buf)
                .unwrap()
        });
        assert_eq!(&buf, &[0xfau8]);

        let mut stdout = io.stdout().unwrap();
        buf[0] = 0xce;
        io.stdout
            .as_ref()
            .map(|v| File::from(v.try_clone_wr().unwrap()).write(&buf).unwrap());
        buf[0] = 0x0;
        stdout.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, &[0xceu8]);

        let mut stderr = io.stderr().unwrap();
        buf[0] = 0xa5;
        io.stderr
            .as_ref()
            .map(|v| File::from(v.try_clone_wr().unwrap()).write(&buf).unwrap());
        buf[0] = 0x0;
        stderr.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, &[0xa5u8]);

        io.close_after_start();
        stdout.read_exact(&mut buf).unwrap_err();
        stderr.read_exact(&mut buf).unwrap_err();
    }

    #[test]
    fn null_io_accepts_child_output() {
        let io = NullIo::new().unwrap();
        let mut command = Command::new("sh");
        command.args(["-c", "printf stdout && printf stderr >&2"]);
        io.set(&mut command).unwrap();
        let mut child = command.spawn().unwrap();
        io.close_after_start();
        assert!(child.wait().unwrap().success());
    }
}
