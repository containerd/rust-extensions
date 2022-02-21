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
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam::sync::WaitGroup;
use log::debug;

use containerd_shim::util::IntoOption;
use containerd_shim::{
    error::{Error, Result},
    io_error,
};
use runc::io::{Io, NullIo, FIFO};

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
                            drop(closer);
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
        let nio = NullIo::new().map_err(io_error!(e, "new Null Io"))?;
        let pio = ProcessIO {
            uri: None,
            io: Some(Arc::new(nio)),
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
