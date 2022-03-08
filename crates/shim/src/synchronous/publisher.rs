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

//! Implements a client to publish events from the shim back to containerd.

use protobuf::Message;

use containerd_shim_protos as client;

use client::protobuf;
use client::shim::events;
use client::ttrpc::{self, context::Context};
use client::types::empty;
use client::{Client, Events, EventsClient};

use crate::error::Result;
use crate::util::{connect, convert_to_any, timestamp};

/// Remote publisher connects to containerd's TTRPC endpoint to publish events from shim.
pub struct RemotePublisher {
    client: EventsClient,
}

impl RemotePublisher {
    /// Connect to containerd's TTRPC endpoint.
    ///
    /// containerd uses `/run/containerd/containerd.sock.ttrpc` by default
    pub fn new(address: impl AsRef<str>) -> Result<RemotePublisher> {
        let client = Self::connect(address)?;

        Ok(RemotePublisher {
            client: EventsClient::new(client),
        })
    }

    fn connect(address: impl AsRef<str>) -> Result<Client> {
        let fd = connect(address)?;
        // Client::new() takes ownership of the RawFd.
        Ok(Client::new(fd))
    }

    /// Publish a new event.
    ///
    /// Event object can be anything that Protobuf able serialize (e.g. implement `Message` trait).
    pub fn publish(
        &self,
        ctx: Context,
        topic: &str,
        namespace: &str,
        event: Box<dyn Message>,
    ) -> Result<()> {
        let mut envelope = events::Envelope::new();
        envelope.set_topic(topic.to_owned());
        envelope.set_namespace(namespace.to_owned());
        envelope.set_timestamp(timestamp()?);
        envelope.set_event(convert_to_any(event)?);

        let mut req = events::ForwardRequest::new();
        req.set_envelope(envelope);

        self.client.forward(ctx, &req)?;

        Ok(())
    }
}

impl Events for RemotePublisher {
    fn forward(
        &self,
        _ctx: &ttrpc::TtrpcContext,
        req: events::ForwardRequest,
    ) -> ttrpc::Result<empty::Empty> {
        self.client.forward(Context::default(), &req)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Barrier};

    use ttrpc::Server;

    use client::api::{Empty, ForwardRequest};
    use client::events::task::TaskOOM;

    use super::*;

    struct FakeServer {}

    impl Events for FakeServer {
        fn forward(&self, _ctx: &ttrpc::TtrpcContext, req: ForwardRequest) -> ttrpc::Result<Empty> {
            let env = req.get_envelope();
            assert_eq!(env.get_topic(), "/tasks/oom");
            Ok(Empty::default())
        }
    }

    #[test]
    fn test_connect() {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = format!("{}/socket", tmpdir.as_ref().to_str().unwrap());
        let path1 = path.clone();

        assert!(RemotePublisher::connect("a".repeat(16384)).is_err());
        assert!(RemotePublisher::connect(&path).is_err());

        let barrier = Arc::new(Barrier::new(2));
        let barrier2 = barrier.clone();
        let thread = std::thread::spawn(move || {
            let listener = UnixListener::bind(&path1).unwrap();
            listener.set_nonblocking(true).unwrap();
            let t = Arc::new(Box::new(FakeServer {}) as Box<dyn Events + Send + Sync>);
            let service = client::create_events(t);
            let mut server = Server::new()
                .add_listener(listener.as_raw_fd())
                .unwrap()
                .register_service(service);
            std::mem::forget(listener);

            server.start().unwrap();
            barrier2.wait();

            barrier2.wait();
            server.shutdown();
        });

        barrier.wait();
        let client = RemotePublisher::new(&path).unwrap();
        let mut msg = TaskOOM::new();
        msg.set_container_id("test".to_string());
        client
            .publish(Context::default(), "/tasks/oom", "ns1", Box::new(msg))
            .unwrap();
        barrier.wait();

        thread.join().unwrap();
    }
}
