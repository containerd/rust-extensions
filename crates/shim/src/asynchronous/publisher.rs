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

use std::os::unix::io::RawFd;

use async_trait::async_trait;
use containerd_shim_protos::{
    api::Empty,
    protobuf::MessageDyn,
    shim::events,
    shim_async::{Client, Events, EventsClient},
    ttrpc,
    ttrpc::{context::Context, r#async::TtrpcContext},
};

use crate::{
    error::Result,
    util::{asyncify, connect, convert_to_any, timestamp},
};

/// Async Remote publisher connects to containerd's TTRPC endpoint to publish events from shim.
pub struct RemotePublisher {
    client: EventsClient,
}

impl RemotePublisher {
    /// Connect to containerd's TTRPC endpoint asynchronously.
    ///
    /// containerd uses `/run/containerd/containerd.sock.ttrpc` by default
    pub async fn new(address: impl AsRef<str>) -> Result<RemotePublisher> {
        let client = Self::connect(address).await?;

        Ok(RemotePublisher {
            client: EventsClient::new(client),
        })
    }

    async fn connect(address: impl AsRef<str>) -> Result<Client> {
        let addr = address.as_ref().to_string();
        let fd = asyncify(move || -> Result<RawFd> {
            let fd = connect(addr)?;
            Ok(fd)
        })
        .await?;

        // Client::new() takes ownership of the RawFd.
        Ok(Client::new(fd))
    }

    /// Publish a new event.
    ///
    /// Event object can be anything that Protobuf able serialize (e.g. implement `Message` trait).
    pub async fn publish(
        &self,
        ctx: Context,
        topic: &str,
        namespace: &str,
        event: Box<dyn MessageDyn>,
    ) -> Result<()> {
        let mut envelope = events::Envelope::new();
        envelope.set_topic(topic.to_owned());
        envelope.set_namespace(namespace.to_owned());
        envelope.set_timestamp(timestamp()?);
        envelope.set_event(convert_to_any(event)?);

        let mut req = events::ForwardRequest::new();
        req.set_envelope(envelope);

        self.client.forward(ctx, &req).await?;

        Ok(())
    }
}

#[async_trait]
impl Events for RemotePublisher {
    async fn forward(
        &self,
        _ctx: &TtrpcContext,
        req: events::ForwardRequest,
    ) -> ttrpc::Result<Empty> {
        self.client.forward(Context::default(), &req).await
    }
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::{io::AsRawFd, net::UnixListener},
        sync::Arc,
    };

    use containerd_shim_protos::{
        api::{Empty, ForwardRequest},
        events::task::TaskOOM,
        shim_async::create_events,
        ttrpc::asynchronous::Server,
    };
    use tokio::sync::{
        mpsc::{channel, Sender},
        Barrier,
    };

    use super::*;

    struct FakeServer {
        tx: Sender<i32>,
    }

    #[async_trait]
    impl Events for FakeServer {
        async fn forward(&self, _ctx: &TtrpcContext, req: ForwardRequest) -> ttrpc::Result<Empty> {
            let env = req.envelope();
            if env.topic() == "/tasks/oom" {
                self.tx.send(0).await.unwrap();
            } else {
                self.tx.send(-1).await.unwrap();
            }
            Ok(Empty::default())
        }
    }

    #[tokio::test]
    async fn test_connect() {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = format!("{}/socket", tmpdir.as_ref().to_str().unwrap());
        let path1 = path.clone();

        assert!(RemotePublisher::connect("a".repeat(16384)).await.is_err());
        assert!(RemotePublisher::connect(&path).await.is_err());

        let (tx, mut rx) = channel(1);
        let server = FakeServer { tx };
        let barrier = Arc::new(Barrier::new(2));
        let barrier2 = barrier.clone();
        let server_thread = tokio::spawn(async move {
            let listener = UnixListener::bind(&path1).unwrap();
            let service = create_events(Arc::new(server));
            let mut server = Server::new()
                .set_domain_unix()
                .add_listener(listener.as_raw_fd())
                .unwrap()
                .register_service(service);
            std::mem::forget(listener);
            server.start().await.unwrap();
            barrier2.wait().await;

            barrier2.wait().await;
            server.shutdown().await.unwrap();
        });

        barrier.wait().await;
        let client = RemotePublisher::new(&path).await.unwrap();
        let mut msg = TaskOOM::new();
        msg.set_container_id("test".to_string());
        client
            .publish(Context::default(), "/tasks/oom", "ns1", Box::new(msg))
            .await
            .unwrap();
        match rx.recv().await {
            Some(0) => {}
            _ => {
                panic!("the received event is not same as published")
            }
        }
        barrier.wait().await;
        server_thread.await.unwrap();
    }
}
