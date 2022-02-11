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

use std::process::Output;

use async_trait::async_trait;
use log::error;
use time::OffsetDateTime;
use tokio::sync::oneshot::{Receiver, Sender};

// ProcessMonitor for handling runc process exit
// Implementation is different from Go's, because if you return Sender in start() and want to
// use it in wait(), then start and wait cannot be executed concurrently.
// Alternatively, caller of start() and wait() have to prepare channel
#[async_trait]
pub trait ProcessMonitor {
    /// Caller cand choose [`std::mem::forget`] about resource
    /// associated to that command, e.g. file descriptors.
    async fn start(
        &self,
        mut cmd: tokio::process::Command,
        tx: Sender<Exit>,
    ) -> std::io::Result<Output> {
        let chi = cmd.spawn()?;
        let pid = chi
            .id()
            .expect("failed to take pid of the container process.");
        let out = chi.wait_with_output().await?;
        let ts = OffsetDateTime::now_utc();
        match tx.send(Exit {
            ts,
            pid,
            status: out.status.code().unwrap(),
        }) {
            Ok(_) => Ok(out),
            Err(e) => {
                error!("command {:?} exited but receiver dropped.", cmd);
                error!("couldn't send messages: {:?}", e);
                Err(std::io::ErrorKind::ConnectionRefused.into())
            }
        }
    }
    async fn wait(&self, rx: Receiver<Exit>) -> std::io::Result<Exit> {
        rx.await.map_err(|_| {
            error!("sender dropped.");
            std::io::ErrorKind::BrokenPipe.into()
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct DefaultMonitor {}

impl ProcessMonitor for DefaultMonitor {}

impl DefaultMonitor {
    pub const fn new() -> Self {
        Self {}
    }
}

#[derive(Debug)]
pub struct Exit {
    pub ts: OffsetDateTime,
    pub pid: u32,
    pub status: i32,
}
