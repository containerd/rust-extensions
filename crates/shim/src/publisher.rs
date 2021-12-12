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

use std::time::{SystemTime, UNIX_EPOCH};

use containerd_shim_client as client;

use client::protobuf;
use client::shim::{empty, events};
use client::ttrpc::{self, context::Context};
use client::{Client, Events, EventsClient};

use protobuf::well_known_types::{Any, Timestamp};
use protobuf::Message;

use thiserror::Error;

/// Remote publisher connects to containerd's TTRPC endpoint to publish events from shim.
pub struct RemotePublisher {
    client: EventsClient,
}

impl RemotePublisher {
    /// Connect to containerd's TTRPC endpoint.
    ///
    /// containerd uses `/run/containerd/containerd.sock.ttrpc` by default
    pub fn new(address: impl AsRef<str>) -> Result<RemotePublisher, Error> {
        let client = Self::connect(address)?;

        Ok(RemotePublisher {
            client: EventsClient::new(client),
        })
    }

    fn connect(address: impl AsRef<str>) -> Result<Client, nix::Error> {
        use nix::sys::socket::*;
        use nix::unistd::close;

        let unix_addr = UnixAddr::new(address.as_ref())?;
        let sock_addr = SockAddr::Unix(unix_addr);

        // SOCK_CLOEXEC flag is Linux specific
        #[cfg(target_os = "linux")]
        const SOCK_CLOEXEC: SockFlag = SockFlag::SOCK_CLOEXEC;

        #[cfg(not(target_os = "linux"))]
        const SOCK_CLOEXEC: SockFlag = SockFlag::empty();

        let fd = socket(AddressFamily::Unix, SockType::Stream, SOCK_CLOEXEC, None)?;

        // MacOS doesn't support atomic creation of a socket descriptor with `SOCK_CLOEXEC` flag,
        // so there is a chance of leak if fork + exec happens in between of these calls.
        #[cfg(not(target_os = "linux"))]
        {
            use nix::fcntl::{fcntl, FcntlArg, FdFlag};
            fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(|e| {
                let _ = close(fd);
                e
            })?;
        }

        connect(fd, &sock_addr).map_err(|e| {
            let _ = close(fd);
            e
        })?;

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
        event: impl Message,
    ) -> Result<(), Error> {
        let mut envelope = events::Envelope::new();
        envelope.set_topic(topic.to_owned());
        envelope.set_namespace(namespace.to_owned());
        envelope.set_timestamp(Self::timestamp()?);
        envelope.set_event(Self::any(event)?);

        let mut req = events::ForwardRequest::new();
        req.set_envelope(envelope);

        self.client.forward(ctx, &req)?;

        Ok(())
    }

    fn timestamp() -> Result<Timestamp, Error> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?;

        let mut ts = Timestamp::default();
        ts.set_seconds(now.as_secs() as _);
        ts.set_nanos(now.subsec_nanos() as _);

        Ok(ts)
    }

    fn any(event: impl Message) -> Result<Any, Error> {
        let data = event.write_to_bytes()?;
        let mut any = Any::new();
        any.merge_from_bytes(&data)?;

        Ok(any)
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

/// Errors returned from client if something went wrong.
#[derive(Debug, Error)]
pub enum Error {
    #[error("Publisher TTRPC error: {0}")]
    Ttrpc(#[from] client::ttrpc::Error),
    #[error("Failed to get envelope timestamp: {0}")]
    Timestamp(#[from] std::time::SystemTimeError),
    #[error("Failed to serialize event: {0}")]
    Any(#[from] protobuf::ProtobufError),
    #[error("Nix error: {0}")]
    Nix(#[from] nix::Error),
}
