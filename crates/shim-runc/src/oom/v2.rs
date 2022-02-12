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

use std::io;
use std::sync::{Arc, Mutex};

use containerd_shim as shim;
use containerd_shim_protos as proto;

use proto::events::task::TaskOOM;
use shim::RemotePublisher;

use cgroups_rs as cgroup;

use cgroup::Hierarchy;
use log::error;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use ttrpc::context::Context;

use super::Watcher;

pub struct WatcherV2 {
    // watcher instance can run() once.
    // receriver will be passed to watcher thread(see run() implementation below).
    tokio_runtime: Runtime,
    item_tx: UnboundedSender<Item>,
    item_rx: Option<UnboundedReceiver<Item>>,
}

struct Item {
    id: String,
    namespace: String,
}

impl WatcherV2 {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<Item>();
        Self {
            tokio_runtime: Builder::new_multi_thread().enable_time().build().unwrap(),
            item_tx: tx,
            item_rx: Some(rx),
        }
    }
}

impl Watcher for WatcherV2 {
    fn run(&mut self, publisher: Arc<Mutex<RemotePublisher>>) -> io::Result<()> {
        let mut rx = if let Some(rx) = self.item_rx.take() {
            rx
        } else {
            error!("WatcherV2::run() called twice: OOM watcher is allow to run only once per instance.");
            return Err(io::ErrorKind::Other.into());
        };

        self.tokio_runtime.block_on(async {
            tokio::spawn(async move {
                // this implementation doesn't receive oom events concurrently
                // but it would be overdoing to spawn task on each recv().
                loop {
                    if let Some(item) = rx.recv().await {
                        let event = TaskOOM {
                            container_id: item.id,
                            ..Default::default()
                        };
                        let ctx = Context::default();
                        match publisher
                            .lock()
                            .unwrap()
                            .publish(ctx, "OOM", &item.namespace, event)
                        {
                            Ok(_) => {}
                            Err(e) => {
                                error!("failed to publish oom event: {}", e);
                            }
                        }
                    }
                }
            });
        });
        Ok(())
    }

    fn add(&self, id: String, namespace: String, cg: Arc<dyn Hierarchy>) -> io::Result<()> {
        if !cg.v2() {
            error!("expected watcher for cgroup v2, v1 found.");
            return Err(io::ErrorKind::Other.into());
        }

        let rx = match cgroup::events::notify_on_oom_v2(&id, &cg.root()) {
            Ok(rx) => rx,
            Err(e) => {
                error!("error on spawning oom watcher: {}", e);
                // only nofity when watcher spawning fails.
                return Ok(());
            }
        };

        let tx = self.item_tx.clone();
        self.tokio_runtime.block_on(async {
            // FIXME: obviously this recv() block and takes up tokio runtime's resources
            tokio::spawn(async move {
                match rx.recv() {
                    Ok(id) => {
                        let item = Item { id, namespace };
                        if let Err(e) = tx.send(item) {
                            error!("OOM watcher: channel disconnected for id={}", e.0.id);
                        }
                    }
                    Err(_) => {
                        error!("OOM watcher: channel disconnected for id={}", id);
                    }
                }
            });
        });
        Ok(())
    }
}
