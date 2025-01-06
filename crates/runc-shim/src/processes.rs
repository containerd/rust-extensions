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
    os::unix::io::AsRawFd,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use containerd_shim::{
    ioctl_set_winsz,
    protos::{
        api::{ProcessInfo, StateResponse, Status},
        cgroups::metrics::Metrics,
        protobuf::well_known_types::timestamp::Timestamp,
    },
    util::asyncify,
    Console, Result,
};
use oci_spec::runtime::LinuxResources;
use time::OffsetDateTime;
use tokio::{
    fs::File,
    sync::oneshot::{channel, Receiver, Sender},
};

use crate::io::Stdio;

#[allow(dead_code)]
#[async_trait]
pub trait Process {
    async fn start(&mut self) -> Result<()>;
    async fn set_exited(&mut self, exit_code: i32);
    async fn pid(&self) -> i32;
    async fn state(&self) -> Result<StateResponse>;
    async fn kill(&mut self, signal: u32, all: bool) -> Result<()>;
    async fn delete(&mut self) -> Result<()>;
    async fn wait_channel(&mut self) -> Result<Receiver<()>>;
    async fn exit_code(&self) -> i32;
    async fn exited_at(&self) -> Option<OffsetDateTime>;
    async fn resize_pty(&mut self, height: u32, width: u32) -> Result<()>;
    async fn update(&mut self, resources: &LinuxResources) -> Result<()>;
    async fn stats(&self) -> Result<Metrics>;
    async fn ps(&self) -> Result<Vec<ProcessInfo>>;
    async fn close_io(&mut self) -> Result<()>;
    async fn pause(&mut self) -> Result<()>;
    async fn resume(&mut self) -> Result<()>;
    async fn id(&self) -> &str;
}

#[async_trait]
pub trait ProcessLifecycle<P: Process> {
    async fn start(&self, p: &mut P) -> Result<()>;
    async fn kill(&self, p: &mut P, signal: u32, all: bool) -> Result<()>;
    async fn delete(&self, p: &mut P) -> Result<()>;
    async fn update(&self, p: &mut P, resources: &LinuxResources) -> Result<()>;
    async fn stats(&self, p: &P) -> Result<Metrics>;
    async fn ps(&self, p: &P) -> Result<Vec<ProcessInfo>>;
    async fn pause(&self, p: &mut P) -> Result<()>;
    async fn resume(&self, p: &mut P) -> Result<()>;
}

pub struct ProcessTemplate<S> {
    pub state: Status,
    pub id: String,
    pub stdio: Stdio,
    pub pid: i32,
    pub exit_code: i32,
    pub exited_at: Option<OffsetDateTime>,
    pub wait_chan_tx: Vec<Sender<()>>,
    pub console: Option<Console>,
    pub lifecycle: Arc<S>,
    pub stdin: Arc<Mutex<Option<File>>>,
}

impl<S> ProcessTemplate<S> {
    pub fn new(id: &str, stdio: Stdio, lifecycle: S) -> Self {
        Self {
            state: Status::CREATED,
            id: id.to_string(),
            stdio,
            pid: 0,
            exit_code: 0,
            exited_at: None,
            wait_chan_tx: vec![],
            console: None,
            lifecycle: Arc::new(lifecycle),
            stdin: Arc::new(Mutex::new(None)),
        }
    }
}

#[async_trait]
impl<S> Process for ProcessTemplate<S>
where
    S: ProcessLifecycle<Self> + Sync + Send,
{
    async fn start(&mut self) -> Result<()> {
        self.lifecycle.clone().start(self).await?;
        Ok(())
    }

    async fn set_exited(&mut self, exit_code: i32) {
        self.state = Status::STOPPED;
        self.exit_code = exit_code;
        self.exited_at = Some(OffsetDateTime::now_utc());
        // set wait_chan_tx to empty, to trigger the drop of the initialized Receiver.
        self.wait_chan_tx = vec![];
    }

    async fn pid(&self) -> i32 {
        self.pid
    }

    async fn id(&self) -> &str {
        self.id.as_str()
    }

    async fn state(&self) -> Result<StateResponse> {
        let mut resp = StateResponse::new();
        resp.id = self.id.to_string();
        resp.set_status(self.state);
        resp.pid = self.pid as u32;
        resp.terminal = self.stdio.terminal;
        resp.stdin = self.stdio.stdin.to_string();
        resp.stdout = self.stdio.stdout.to_string();
        resp.stderr = self.stdio.stderr.to_string();
        resp.exit_status = self.exit_code as u32;
        if let Some(exit_at) = self.exited_at {
            let mut time_stamp = Timestamp::new();
            time_stamp.seconds = exit_at.unix_timestamp();
            time_stamp.nanos = exit_at.nanosecond() as i32;
            resp.exited_at = Some(time_stamp).into();
        }
        Ok(resp)
    }

    async fn kill(&mut self, signal: u32, all: bool) -> Result<()> {
        self.lifecycle.clone().kill(self, signal, all).await
    }

    async fn delete(&mut self) -> Result<()> {
        self.lifecycle.clone().delete(self).await
    }

    async fn wait_channel(&mut self) -> Result<Receiver<()>> {
        let (tx, rx) = channel::<()>();
        if self.state != Status::STOPPED {
            self.wait_chan_tx.push(tx);
        }
        Ok(rx)
    }

    async fn exit_code(&self) -> i32 {
        self.exit_code
    }

    async fn exited_at(&self) -> Option<OffsetDateTime> {
        self.exited_at
    }

    async fn resize_pty(&mut self, height: u32, width: u32) -> Result<()> {
        if let Some(console) = self.console.as_ref() {
            let w = libc::winsize {
                ws_row: height as u16,
                ws_col: width as u16,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            let fd = console.file.as_raw_fd();
            asyncify(move || -> Result<()> {
                unsafe { ioctl_set_winsz(fd, &w).map(|_x| ()).map_err(Into::into) }
            })
            .await?;
        }
        Ok(())
    }

    async fn update(&mut self, resources: &LinuxResources) -> Result<()> {
        self.lifecycle.clone().update(self, resources).await
    }

    async fn stats(&self) -> Result<Metrics> {
        self.lifecycle.stats(self).await
    }

    async fn ps(&self) -> Result<Vec<ProcessInfo>> {
        self.lifecycle.ps(self).await
    }

    async fn close_io(&mut self) -> Result<()> {
        let mut lock_guard = self.stdin.lock().unwrap();
        if let Some(stdin_w_file) = lock_guard.take() {
            drop(stdin_w_file);
        }
        Ok(())
    }

    async fn pause(&mut self) -> Result<()> {
        self.lifecycle.clone().pause(self).await
    }

    async fn resume(&mut self) -> Result<()> {
        self.lifecycle.clone().resume(self).await
    }
}
