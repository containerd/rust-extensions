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

use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Write};
use std::thread::JoinHandle;

use crossbeam::sync::WaitGroup;
use log::debug;

use containerd_shim::io::Stdio;
use containerd_shim::{
    error::{Error, Result},
    io_error,
};

use crate::common::ProcessIO;

const DEFAULT_BUF_SIZE: usize = 8 * 1024;

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

// Special copy io stream form/to tty
pub fn spawn_copy_for_tty<R: Read + Send + 'static, W: Write + Send + 'static>(
    mut from: R,
    mut to: W,
    wg_opt: Option<&WaitGroup>,
    on_close_opt: Option<Box<dyn FnOnce() + Send + Sync>>,
) -> JoinHandle<()> {
    let wg_opt_clone = wg_opt.cloned();
    std::thread::spawn(move || {
        if let Err(e) = copy(&mut from, &mut to) {
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

pub fn copy<R: ?Sized, W: ?Sized>(reader: &mut R, writer: &mut W) -> std::io::Result<u64>
where
    R: Read,
    W: Write,
{
    let mut buf = [0u8; DEFAULT_BUF_SIZE];
    let mut written = 0;

    loop {
        let len = match reader.read(&mut buf) {
            Ok(0) => return Ok(written),
            Ok(len) => len,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };

        written += len as u64;
        writer.write_all(&buf[..len])?;
    }
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
                    spawn_copy(stdin, w, None, None);
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
