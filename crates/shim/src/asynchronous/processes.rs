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

use std::os::unix::io::AsRawFd;
use std::sync::Arc;

use async_trait::async_trait;
use oci_spec::runtime::LinuxResources;
use time::OffsetDateTime;
use tokio::sync::oneshot::{channel, Receiver, Sender};

use containerd_shim_protos::api::{ProcessInfo, StateResponse, Status};
use containerd_shim_protos::cgroups::metrics::Metrics;
use containerd_shim_protos::protobuf::well_known_types::Timestamp;

use crate::io::Stdio;
use crate::util::asyncify;
use crate::{ioctl_set_winsz, Console};

#[async_trait]
pub trait Process {
    async fn start(&mut self) -> crate::Result<()>;
    async fn set_exited(&mut self, exit_code: i32);
    async fn pid(&self) -> i32;
    async fn state(&self) -> crate::Result<StateResponse>;
    async fn kill(&mut self, signal: u32, all: bool) -> crate::Result<()>;
    async fn delete(&mut self) -> crate::Result<()>;
    async fn wait_channel(&mut self) -> crate::Result<Receiver<()>>;
    async fn exit_code(&self) -> i32;
    async fn exited_at(&self) -> Option<OffsetDateTime>;
    async fn resize_pty(&mut self, height: u32, width: u32) -> crate::Result<()>;
    async fn update(&mut self, resources: &LinuxResources) -> crate::Result<()>;
    async fn stats(&self) -> crate::Result<Metrics>;
    async fn ps(&self) -> crate::Result<Vec<ProcessInfo>>;
}

#[async_trait]
pub trait ProcessLifecycle<P: Process> {
    async fn start(&self, p: &mut P) -> crate::Result<()>;
    async fn kill(&self, p: &mut P, signal: u32, all: bool) -> crate::Result<()>;
    async fn delete(&self, p: &mut P) -> crate::Result<()>;
    async fn update(&self, p: &mut P, resources: &LinuxResources) -> crate::Result<()>;
    async fn stats(&self, p: &P) -> crate::Result<Metrics>;
    async fn ps(&self, p: &P) -> crate::Result<Vec<ProcessInfo>>;
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
        }
    }
}

#[async_trait]
impl<S> Process for ProcessTemplate<S>
where
    S: ProcessLifecycle<Self> + Sync + Send,
{
    async fn start(&mut self) -> crate::Result<()> {
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

    async fn state(&self) -> crate::Result<StateResponse> {
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
        Ok(resp)
    }

    async fn kill(&mut self, signal: u32, all: bool) -> crate::Result<()> {
        self.lifecycle.clone().kill(self, signal, all).await
    }

    async fn delete(&mut self) -> crate::Result<()> {
        self.lifecycle.clone().delete(self).await
    }

    async fn wait_channel(&mut self) -> crate::Result<Receiver<()>> {
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

    async fn resize_pty(&mut self, height: u32, width: u32) -> crate::Result<()> {
        if let Some(console) = self.console.as_ref() {
            let w = libc::winsize {
                ws_row: height as u16,
                ws_col: width as u16,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            let fd = console.file.as_raw_fd();
            asyncify(move || -> crate::Result<()> {
                unsafe { ioctl_set_winsz(fd, &w).map(|_x| ()).map_err(Into::into) }
            })
            .await?;
        }
        Ok(())
    }

    async fn update(&mut self, resources: &LinuxResources) -> crate::Result<()> {
        self.lifecycle.clone().update(self, resources).await
    }

    async fn stats(&self) -> crate::Result<Metrics> {
        self.lifecycle.stats(self).await
    }

    async fn ps(&self) -> crate::Result<Vec<ProcessInfo>> {
        self.lifecycle.ps(self).await
    }
}
