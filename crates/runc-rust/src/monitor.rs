/*
   copyright the containerd authors.

   licensed under the apache license, version 2.0 (the "license");
   you may not use this file except in compliance with the license.
   you may obtain a copy of the license at

       http://www.apache.org/licenses/license-2.0

   unless required by applicable law or agreed to in writing, software
   distributed under the license is distributed on an "as is" basis,
   without warranties or conditions of any kind, either express or implied.
   see the license for the specific language governing permissions and
   limitations under the license.
*/

use std::process::Output;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use log::error;
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
        forget: bool,
    ) -> std::io::Result<Output> {
        let chi = cmd.spawn()?;
        let pid = chi
            .id()
            .expect("failed to take pid of the container process.");
        let out = chi.wait_with_output().await?;
        let ts = Utc::now();
        match tx.send(Exit {
            ts,
            pid,
            status: out.status.code().unwrap(),
        }) {
            Ok(_) => {
                if forget {
                    std::mem::forget(cmd);
                }
                Ok(out)
            }
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
    pub ts: DateTime<Utc>,
    pub pid: u32,
    pub status: i32,
}
