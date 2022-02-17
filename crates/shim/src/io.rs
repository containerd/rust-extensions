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
use std::fmt::Debug;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::process::Command;
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam::sync::WaitGroup;
use log::debug;
use nix::sys::termios::Termios;

use crate::error::{Error, Result};
use crate::util::IntoOption;

pub trait WriteCloser: Write {
    fn close(&self);
}

pub trait Io: Sync + Send {
    /// Return write side of stdin
    fn stdin(&self) -> Option<Box<dyn WriteCloser + Send + Sync>>;

    /// Return read side of stdout
    fn stdout(&self) -> Option<Box<dyn Read + Send>>;

    /// Return read side of stderr
    fn stderr(&self) -> Option<Box<dyn Read + Send>>;

    /// Set IO for passed command.
    /// Read side of stdin, write side of stdout and write side of stderr should be provided to command.
    fn set(&self, cmd: &mut Command) -> Result<()>;

    /// Only close write side (should be stdout/err "from" runc process)
    fn close_after_start(&self);
}

pub struct NullIo {}

impl Io for NullIo {
    fn stdin(&self) -> Option<Box<dyn WriteCloser + Send + Sync>> {
        None
    }

    fn stdout(&self) -> Option<Box<dyn Read + Send>> {
        None
    }

    fn stderr(&self) -> Option<Box<dyn Read + Send>> {
        None
    }

    fn set(&self, cmd: &mut Command) -> Result<()> {
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        Ok(())
    }

    fn close_after_start(&self) {}
}

pub struct FIFO {
    pub stdin: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

impl Io for FIFO {
    fn stdin(&self) -> Option<Box<dyn WriteCloser + Send + Sync>> {
        None
    }

    fn stdout(&self) -> Option<Box<dyn Read + Send>> {
        None
    }

    fn stderr(&self) -> Option<Box<dyn Read + Send>> {
        None
    }

    fn set(&self, cmd: &mut Command) -> Result<()> {
        let r: Option<std::io::Result<()>> = self.stdin.as_ref().map(|path| {
            let stdin = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)?;
            cmd.stdin(stdin);
            Ok(())
        });
        r.unwrap_or(Ok(())).map_err(io_error!(e, "open stdin"))?;

        let r: Option<std::io::Result<()>> = self.stdout.as_ref().map(|path| {
            let stdout = OpenOptions::new().write(true).open(path)?;
            cmd.stdout(stdout);
            Ok(())
        });
        r.unwrap_or(Ok(())).map_err(io_error!(e, "open stdout"))?;

        let r: Option<std::io::Result<()>> = self.stderr.as_ref().map(|path| {
            let stderr = OpenOptions::new().write(true).open(path)?;
            cmd.stderr(stderr);
            Ok(())
        });
        r.unwrap_or(Ok(())).map_err(io_error!(e, "open stderr"))?;

        Ok(())
    }

    fn close_after_start(&self) {}
}

pub struct Console {
    pub file: File,
    pub termios: Termios,
}

pub fn spawn_copy<R: Read + Send + 'static, W: Write + Send + 'static>(
    mut from: R,
    mut to: W,
    wg_opt: Option<&WaitGroup>,
    on_close_opt: Option<Box<dyn FnOnce() + Send + Sync>>,
) -> JoinHandle<()> {
    let wg_opt_clone = wg_opt.cloned();
    std::thread::spawn(move || {
        if let Err(e) = std::io::copy(&mut from, &mut to) {
            debug!("copy io error: {}", e);
        }
        if let Some(x) = on_close_opt {
            x()
        };
        if let Some(x) = wg_opt_clone {
            std::mem::drop(x)
        };
    })
}

#[derive(Clone, Debug)]
pub struct Stdio {
    pub stdin: String,
    pub stdout: String,
    pub stderr: String,
    pub terminal: bool,
}

impl Stdio {
    pub fn is_null(&self) -> bool {
        self.stdin.is_empty() && self.stdout.is_empty() && self.stderr.is_empty()
    }
}

pub struct ProcessIO {
    pub uri: Option<String>,
    pub io: Option<Arc<dyn Io>>,
    pub copy: bool,
}

impl ProcessIO {
    pub fn copy(&self, stdio: &Stdio) -> Result<WaitGroup> {
        let wg = WaitGroup::new();
        if !self.copy {
            return Ok(wg);
        };
        if let Some(pio) = &self.io {
            if let Some(w) = pio.stdin() {
                debug!("copy_io: pipe stdin from {}", stdio.stdin.as_str());
                if !stdio.stdin.is_empty() {
                    let stdin = OpenOptions::new()
                        .read(true)
                        .open(stdio.stdin.as_str())
                        .map_err(io_error!(e, "open stdin"))?;
                    let closer = pio.stdin().unwrap();
                    spawn_copy(
                        stdin,
                        w,
                        None,
                        Some(Box::new(move || {
                            closer.close();
                        })),
                    );
                }
            }

            if let Some(r) = pio.stdout() {
                debug!("copy_io: pipe stdout from to {}", stdio.stdout.as_str());
                if !stdio.stdout.is_empty() {
                    let stdout = OpenOptions::new()
                        .write(true)
                        .open(stdio.stdout.as_str())
                        .map_err(io_error!(e, "open stdout"))?;
                    // open a read to make sure even if the read end of containerd shutdown,
                    // copy still continue until the restart of containerd succeed
                    let stdout_r = OpenOptions::new()
                        .read(true)
                        .open(stdio.stdout.as_str())
                        .map_err(io_error!(e, "open stdout for read"))?;
                    spawn_copy(
                        r,
                        stdout,
                        Some(&wg),
                        Some(Box::new(move || {
                            drop(stdout_r);
                        })),
                    );
                }
            }

            if let Some(r) = pio.stderr() {
                if !stdio.stderr.is_empty() {
                    debug!("copy_io: pipe stderr from to {}", stdio.stderr.as_str());
                    let stderr = OpenOptions::new()
                        .write(true)
                        .open(stdio.stderr.as_str())
                        .map_err(io_error!(e, "open stderr"))?;
                    // open a read to make sure even if the read end of containerd shutdown,
                    // copy still continue until the restart of containerd succeed
                    let stderr_r = OpenOptions::new()
                        .read(true)
                        .open(stdio.stderr.as_str())
                        .map_err(io_error!(e, "open stderr for read"))?;
                    spawn_copy(
                        r,
                        stderr,
                        Some(&wg),
                        Some(Box::new(move || {
                            drop(stderr_r);
                        })),
                    );
                }
            }
        }

        Ok(wg)
    }
}

pub fn create_io(id: &str, _io_uid: u32, _io_gid: u32, stdio: &Stdio) -> Result<ProcessIO> {
    if stdio.is_null() {
        let pio = ProcessIO {
            uri: None,
            io: Some(Arc::new(NullIo {})),
            copy: false,
        };
        return Ok(pio);
    }
    let stdout = stdio.stdout.as_str();
    let scheme_path = stdout.trim().split("://").collect::<Vec<&str>>();
    let scheme: &str;
    let uri: String;
    if scheme_path.len() <= 1 {
        // no scheme specified
        // default schema to fifo
        uri = format!("fifo://{}", stdout);
        scheme = "fifo"
    } else {
        uri = stdout.to_string();
        scheme = scheme_path[0];
    }

    let mut pio = ProcessIO {
        uri: Some(uri),
        io: None,
        copy: false,
    };

    if scheme == "fifo" {
        debug!(
            "create named pipe io for container {}, stdin: {}, stdout: {}, stderr: {}",
            id,
            stdio.stdin.as_str(),
            stdio.stdout.as_str(),
            stdio.stderr.as_str()
        );
        let io = FIFO {
            stdin: stdio.stdin.to_string().none_if(|x| x.is_empty()),
            stdout: stdio.stdout.to_string().none_if(|x| x.is_empty()),
            stderr: stdio.stderr.to_string().none_if(|x| x.is_empty()),
        };
        pio.io = Some(Arc::new(io));
        pio.copy = false;
    }
    Ok(pio)
}
