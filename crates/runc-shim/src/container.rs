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

use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs::{File, OpenOptions};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::path::Path;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};

use log::{debug, warn};
use nix::sys::socket::{recvmsg, ControlMessageOwned, MsgFlags};
use nix::sys::termios::tcgetattr;
use nix::sys::uio::IoVec;
use nix::{cmsg_space, ioctl_write_ptr_bad};
use runc::console::{Console, ConsoleSocket};
use time::OffsetDateTime;

use containerd_shim as shim;

use shim::api::*;
use shim::error::{Error, Result};
use shim::protos::protobuf::well_known_types::Timestamp;
use shim::util::read_pid_from_file;
use shim::{io_error, other, other_error};

use crate::io::{spawn_copy, ProcessIO, Stdio};

ioctl_write_ptr_bad!(ioctl_set_winsz, libc::TIOCSWINSZ, libc::winsize);

pub trait ContainerFactory<C> {
    fn create(&self, ns: &str, req: &CreateTaskRequest) -> Result<C>;
}

pub trait Process {
    fn set_exited(&mut self, exit_code: i32);
    fn id(&self) -> &str;
    fn status(&self) -> Status;
    fn set_status(&mut self, status: Status);
    fn pid(&self) -> i32;
    fn terminal(&self) -> bool;
    fn stdin(&self) -> String;
    fn stdout(&self) -> String;
    fn stderr(&self) -> String;
    fn state(&self) -> StateResponse;
    fn add_wait(&mut self, tx: SyncSender<i8>);
    fn exit_code(&self) -> i32;
    fn exited_at(&self) -> Option<OffsetDateTime>;
    fn copy_console(&self, console_socket: &ConsoleSocket) -> Result<Console>;
    fn copy_io(&self) -> Result<()>;
    fn set_pid_from_file(&mut self, pid_path: &Path) -> Result<()>;
    fn resize_pty(&mut self, height: u32, width: u32) -> Result<()>;
}

pub trait Container {
    fn start(&mut self, exec_id: Option<&str>) -> Result<i32>;
    fn state(&self, exec_id: Option<&str>) -> Result<StateResponse>;
    fn kill(
        &mut self,
        exec_id: Option<&str>,
        signal: u32,
        all: bool,
    ) -> containerd_shim::Result<()>;
    fn wait_channel(&mut self, exec_id: Option<&str>) -> Result<Receiver<i8>>;
    fn get_exit_info(&self, exec_id: Option<&str>) -> Result<(i32, i32, Option<OffsetDateTime>)>;
    fn delete(&mut self, exec_id_opt: Option<&str>) -> Result<(i32, u32, Timestamp)>;
    fn exec(&mut self, req: ExecProcessRequest) -> Result<()>;
    fn resize_pty(&mut self, exec_id: Option<&str>, height: u32, width: u32) -> Result<()>;
    fn pid(&self) -> i32;
}

pub struct CommonContainer<T, E> {
    pub id: String,
    pub bundle: String,
    pub init: T,
    pub processes: HashMap<String, E>,
}

impl<T, E> CommonContainer<T, E>
where
    T: Process,
    E: Process,
    E: TryFrom<ExecProcessRequest>,
    E::Error: ToString,
{
    pub fn get_process(&self, exec_id: Option<&str>) -> Result<&dyn Process> {
        match exec_id {
            Some(exec_id) => {
                let p = self.processes.get(exec_id).ok_or_else(|| {
                    Error::NotFoundError("can not find the exec by id".to_string())
                })?;
                Ok(p)
            }
            None => Ok(&self.init),
        }
    }

    pub fn get_mut_process(&mut self, exec_id: Option<&str>) -> Result<&mut dyn Process> {
        match exec_id {
            Some(exec_id) => {
                let p = self.processes.get_mut(exec_id).ok_or_else(|| {
                    Error::NotFoundError("can not find the exec by id".to_string())
                })?;
                Ok(p)
            }
            None => Ok(&mut self.init),
        }
    }

    pub fn state(&self, exec_id: Option<&str>) -> Result<StateResponse> {
        let process = self.get_process(exec_id)?;
        let mut resp = process.state();
        resp.bundle = self.bundle.to_string();
        debug!("container state: {:?}", resp);
        Ok(resp)
    }

    #[allow(unused)]
    pub fn exec(&mut self, req: ExecProcessRequest) -> Result<()> {
        let exec_id = req.exec_id.to_string();
        let exec_process = E::try_from(req).map_err(other_error!(e, "convert ExecProcess"))?;
        self.processes.insert(exec_id, exec_process);
        Ok(())
    }

    pub fn wait_channel(&mut self, exec_id: Option<&str>) -> Result<Receiver<i8>> {
        let process = self.get_mut_process(exec_id)?;
        let (tx, rx) = sync_channel::<i8>(0);
        if process.exited_at() == None {
            process.add_wait(tx);
        }
        Ok(rx)
    }

    pub fn get_exit_info(
        &self,
        exec_id: Option<&str>,
    ) -> Result<(i32, i32, Option<OffsetDateTime>)> {
        let process = self.get_process(exec_id)?;
        Ok((process.pid(), process.exit_code(), process.exited_at()))
    }

    pub fn resize_pty(&mut self, exec_id: Option<&str>, height: u32, width: u32) -> Result<()> {
        match exec_id {
            Some(exec_id) => {
                let process = self.processes.get_mut(exec_id).ok_or_else(|| {
                    Error::NotFoundError("can not find the exec by id".to_string())
                })?;
                process.resize_pty(height, width)?;
                Ok(())
            }
            None => Ok(()),
        }
    }
}

pub struct CommonProcess {
    pub state: Status,
    pub id: String,
    pub stdio: Stdio,
    pub pid: i32,
    pub io: Option<ProcessIO>,
    pub exit_code: i32,
    pub exited_at: Option<OffsetDateTime>,
    pub wait_chan_tx: Vec<SyncSender<i8>>,
    pub console: Option<Console>,
}

impl Process for CommonProcess {
    fn set_exited(&mut self, exit_code: i32) {
        self.state = Status::STOPPED;
        self.exit_code = exit_code;
        self.exited_at = Some(OffsetDateTime::now_utc());
        // set wait_chan_tx to empty, to trigger the drop of the initialized Receiver.
        self.wait_chan_tx = vec![];
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn status(&self) -> Status {
        self.state
    }

    fn set_status(&mut self, status: Status) {
        self.state = status;
    }

    fn pid(&self) -> i32 {
        self.pid
    }

    fn terminal(&self) -> bool {
        self.stdio.terminal
    }

    fn stdin(&self) -> String {
        self.stdio.stdin.to_string()
    }

    fn stdout(&self) -> String {
        self.stdio.stdout.to_string()
    }

    fn stderr(&self) -> String {
        self.stdio.stderr.to_string()
    }

    fn state(&self) -> StateResponse {
        let mut resp = StateResponse::new();
        resp.id = self.id.to_string();
        resp.status = self.state;
        resp.pid = self.pid as u32;
        resp.terminal = self.stdio.terminal;
        resp.stdin = self.stdio.stdin.to_string();
        resp.stdout = self.stdio.stdout.to_string();
        resp.stderr = self.stdio.stderr.to_string();
        resp.exit_status = self.exit_code as u32;
        if let Some(exit_at) = self.exited_at {
            let mut time_stamp = Timestamp::new();
            time_stamp.set_seconds(exit_at.unix_timestamp());
            time_stamp.set_nanos(exit_at.nanosecond() as i32);
            resp.set_exited_at(time_stamp);
        }
        resp
    }

    fn add_wait(&mut self, tx: SyncSender<i8>) {
        self.wait_chan_tx.push(tx)
    }

    fn exit_code(&self) -> i32 {
        self.exit_code
    }

    fn exited_at(&self) -> Option<OffsetDateTime> {
        self.exited_at
    }

    fn copy_console(&self, console_socket: &ConsoleSocket) -> Result<Console> {
        debug!("copy_console: waiting for runtime to send console fd");
        let stream = console_socket
            .accept()
            .map_err(io_error!(e, "accept console socket"))?;
        let mut buf = [0u8; 4096];
        let iovec = [IoVec::from_mut_slice(&mut buf)];
        let mut space = cmsg_space!([RawFd; 2]);
        let (path, fds) = match recvmsg(
            stream.as_raw_fd(),
            &iovec,
            Some(&mut space),
            MsgFlags::empty(),
        ) {
            Ok(msg) => {
                let mut iter = msg.cmsgs();
                if let Some(ControlMessageOwned::ScmRights(fds)) = iter.next() {
                    (iovec[0].as_slice(), fds)
                } else {
                    return Err(other!("received message is empty"));
                }
            }
            Err(e) => {
                return Err(other!(e, "failed to receive message"));
            }
        };
        if fds.is_empty() {
            return Err(other!("received message is empty"));
        }
        let path = String::from_utf8(Vec::from(path)).unwrap_or_else(|e| {
            warn!("failed to get path from array {}", e);
            "".to_string()
        });
        let path = path.trim_matches(char::from(0));
        debug!(
            "copy_console: console socket get path: {}, fd: {}",
            path, &fds[0]
        );
        let f = unsafe { File::from_raw_fd(fds[0]) };
        let termios = tcgetattr(fds[0])?;

        if !self.stdio.stdin.is_empty() {
            debug!("copy_console: pipe stdin to console");
            let f = unsafe { File::from_raw_fd(fds[0]) };
            let stdin = OpenOptions::new()
                .read(true)
                .write(true)
                .open(self.stdio.stdin.as_str())
                .map_err(io_error!(e, "open stdin"))?;
            spawn_copy(stdin, f, None, None);
        }

        if !self.stdio.stdout.is_empty() {
            let f = unsafe { File::from_raw_fd(fds[0]) };
            debug!("copy_console: pipe stdout from console");
            let stdout = OpenOptions::new()
                .write(true)
                .open(self.stdio.stdout.as_str())
                .map_err(io_error!(e, "open stdout"))?;
            // open a read to make sure even if the read end of containerd shutdown,
            // copy still continue until the restart of containerd succeed
            let stdout_r = OpenOptions::new()
                .read(true)
                .open(self.stdio.stdout.as_str())
                .map_err(io_error!(e, "open stdout for read"))?;
            spawn_copy(
                f,
                stdout,
                None,
                Some(Box::new(move || {
                    drop(stdout_r);
                })),
            );
        }
        let console = Console { file: f, termios };
        Ok(console)
    }

    fn copy_io(&self) -> Result<()> {
        if let Some(pio) = self.io.as_ref() {
            pio.copy(&self.stdio)?;
        };
        Ok(())
    }

    fn set_pid_from_file(&mut self, pid_path: &Path) -> Result<()> {
        let pid = read_pid_from_file(pid_path)?;
        self.pid = pid;
        Ok(())
    }

    fn resize_pty(&mut self, height: u32, width: u32) -> Result<()> {
        match self.console.as_ref() {
            Some(console) => unsafe {
                let w = libc::winsize {
                    ws_row: height as u16,
                    ws_col: width as u16,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                ioctl_set_winsz(console.file.as_raw_fd(), &w)
                    .map(|_x| ())
                    .map_err(Into::into)
            },
            None => Err(other!("there is no console")),
        }
    }
}
