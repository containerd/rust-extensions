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
    fs::OpenOptions,
    io::{ErrorKind, Read, Write},
    os::unix::fs::OpenOptionsExt,
    thread::JoinHandle,
};

use containerd_shim::{
    error::{Error, Result},
    io_error,
};
use crossbeam::sync::WaitGroup;
use log::debug;

use crate::{common::ProcessIO, io::Stdio};

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
    pub fn copy(&mut self, stdio: &Stdio) -> Result<WaitGroup> {
        let wg = WaitGroup::new();
        if !self.copy {
            if !stdio.stdout.is_empty() {
                // Open a reader to make sure even if the read side of containerd closed,
                // the writer won't get EPIPE.
                let stdout_r = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(stdio.stdout.as_str())
                    .map_err(io_error!(e, "failed to open stdout for reading"))?;
                self.stdout_r = Some(stdout_r);
            }
            if !stdio.stderr.is_empty() {
                let stderr_r = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(stdio.stderr.as_str())
                    .map_err(io_error!(e, "failed to open stderr for reading"))?;
                self.stderr_r = Some(stderr_r);
            }

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

#[cfg(test)]
mod tests {
    use std::{
        fs::{remove_file, OpenOptions},
        io::{ErrorKind, Read, Write},
        path::Path,
        thread,
        time::Duration,
    };

    use nix::{sys::stat::Mode, unistd::mkfifo};

    use crate::{common::ProcessIO, io::Stdio};

    // Test writing stderr fifo is ok when no outside reader.
    #[test]
    fn test_set_io_restart() {
        let tmp_fifo_str = "/tmp/shim-test-fifo";
        create_fifo_if_not_exist(tmp_fifo_str);
        let tmp_fifo = Path::new(tmp_fifo_str);

        simulate_containerd_to_open_fifo(tmp_fifo_str.to_string());

        // Simulate container side
        let mut stderr_w = OpenOptions::new()
            .write(true)
            .open(tmp_fifo)
            .expect("failed to open stderr for write");
        assert!(stderr_w.write(b"hello\n").is_ok());

        thread::sleep(Duration::from_millis(100)); // wait until containerd side died
        let err = stderr_w
            .write(b"hello\n")
            .expect_err("should get error when no reader");
        assert_eq!(err.kind(), ErrorKind::BrokenPipe);
        let stdio = Stdio {
            stderr: tmp_fifo_str.to_string(),
            ..Default::default()
        };
        let mut process_io = ProcessIO::default();
        let _ = process_io.copy(&stdio);
        assert!(stderr_w.write(b"hello\n").is_ok());

        // Cleanup resources
        remove_file(tmp_fifo).unwrap_or_default();
    }

    // Test exec with no "it" scenario, container process runs so fast that write side has been closed.
    #[test]
    fn test_set_io_no_writer() {
        let tmp_fifo_str = "/tmp/shim-test-fifo-exec";
        create_fifo_if_not_exist(tmp_fifo_str);
        let tmp_fifo = Path::new(tmp_fifo_str);

        simulate_containerd_to_open_fifo(tmp_fifo_str.to_string());

        // Simulate container side
        let mut stderr_w = OpenOptions::new()
            .write(true)
            .open(tmp_fifo)
            .expect("failed to open stderr for write");
        assert!(stderr_w.write(b"hello\n").is_ok());

        // container exited, close writer
        drop(stderr_w);

        let stdio = Stdio {
            stderr: tmp_fifo_str.to_string(),
            ..Default::default()
        };
        let mut process_io = ProcessIO::default();
        let _ = process_io.copy(&stdio);

        // Cleanup resources
        remove_file(tmp_fifo).unwrap_or_default();
    }

    fn create_fifo_if_not_exist(path: &str) {
        let tmp_fifo = Path::new(path);
        if tmp_fifo.exists() {
            remove_file(tmp_fifo).expect("failed to remove tmp fifo");
        }
        mkfifo(tmp_fifo, Mode::from_bits(0o600).unwrap()).expect("failed to create tmp fifo");
    }

    fn simulate_containerd_to_open_fifo(tmp_fifo_str: String) {
        // Simulate containerd side to open fifo for read
        thread::spawn(move || {
            let tmp_fifo = Path::new(&tmp_fifo_str);
            let mut containerd_r = OpenOptions::new()
                .read(true)
                .open(tmp_fifo)
                .expect("failed to open stderr for read");
            let mut buf = Vec::with_capacity(6);
            thread::sleep(Duration::from_millis(50));
            assert!(containerd_r.read(buf.as_mut()).is_ok());
            // Fd will be closed here
        });
    }
}
