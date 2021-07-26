use std::time::{SystemTime, UNIX_EPOCH};

use containerd_shim_protos as client;

use client::protobuf;
use client::shim::{empty, events};
use client::ttrpc::{self, context::Context};
use client::{Client, Events, EventsClient};

use protobuf::well_known_types::{Any, Timestamp};
use protobuf::Message;

use thiserror::Error;

pub struct RemotePublisher {
    client: EventsClient,
}

impl RemotePublisher {
    pub fn new(address: impl AsRef<str>) -> Result<RemotePublisher, Error> {
        let client = Client::connect(address.as_ref())?;

        Ok(RemotePublisher {
            client: EventsClient::new(client),
        })
    }

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
        ts.set_nanos(now.as_nanos() as _);

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

#[derive(Debug, Error)]
pub enum Error {
    #[error("Publisher TTRPC error")]
    Ttrpc(#[from] containerd_shim_protos::ttrpc::Error),
    #[error("Failed to get envelope timestamp")]
    Timestamp(#[from] std::time::SystemTimeError),
    #[error("Failed to serialize event")]
    Any(#[from] protobuf::ProtobufError),
}
