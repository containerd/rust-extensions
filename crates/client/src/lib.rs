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

#![cfg_attr(feature = "docs", doc = include_str!("../README.md"))]
// No way to derive Eq with tonic :(
// See https://github.com/hyperium/tonic/issues/1056
#![allow(clippy::derive_partial_eq_without_eq)]

pub use tonic;

/// Generated `containerd.types` types.
pub mod types {
    tonic::include_proto!("containerd.types");

    pub mod v1 {
        tonic::include_proto!("containerd.v1.types");
    }
}

/// Generated `google.rpc` types, containerd services typically use some of these types.
pub mod google {
    pub mod rpc {
        tonic::include_proto!("google.rpc");
    }
}

/// Generated `containerd.services.*` services.
pub mod services {
    #[allow(clippy::tabs_in_doc_comments)]
    #[allow(rustdoc::invalid_rust_codeblocks)]
    #[allow(rustdoc::invalid_html_tags)]
    pub mod v1 {
        tonic::include_proto!("containerd.services.containers.v1");
        tonic::include_proto!("containerd.services.content.v1");
        tonic::include_proto!("containerd.services.diff.v1");
        tonic::include_proto!("containerd.services.events.v1");
        tonic::include_proto!("containerd.services.images.v1");
        tonic::include_proto!("containerd.services.introspection.v1");
        tonic::include_proto!("containerd.services.leases.v1");
        tonic::include_proto!("containerd.services.namespaces.v1");
        tonic::include_proto!("containerd.services.streaming.v1");
        tonic::include_proto!("containerd.services.tasks.v1");
        tonic::include_proto!("containerd.services.transfer.v1");

        // Sandbox services (Controller and Store) don't make it clear that they are for sandboxes.
        // Wrap these into a sub module to make the names more clear.
        pub mod sandbox {
            tonic::include_proto!("containerd.services.sandbox.v1");
        }

        // Snapshot's `Info` conflicts with Content's `Info`, so wrap it into a separate sub module.
        pub mod snapshots {
            tonic::include_proto!("containerd.services.snapshots.v1");
        }

        tonic::include_proto!("containerd.services.version.v1");
    }
}

/// Generated event types.
pub mod events {
    tonic::include_proto!("containerd.events");
}

/// Connect creates a unix channel to containerd GRPC socket.
///
/// This helper intended to be used in conjunction with [Tokio](https://tokio.rs) runtime.
#[cfg(feature = "connect")]
pub async fn connect(
    path: impl AsRef<std::path::Path>,
) -> Result<tonic::transport::Channel, tonic::transport::Error> {
    use tonic::transport::Endpoint;

    let path = path.as_ref().to_path_buf();

    // Taken from https://github.com/hyperium/tonic/blob/71fca362d7ffbb230547f23b3f2fb75c414063a8/examples/src/uds/client.rs#L21-L28
    // There will ignore this uri because uds do not use it
    // and make connection with UnixStream::connect.
    let channel = Endpoint::try_from("http://[::]")?
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();

            async move {
                #[cfg(unix)]
                {
                    Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(
                        tokio::net::UnixStream::connect(path).await?,
                    ))
                }

                #[cfg(windows)]
                {
                    let client = tokio::net::windows::named_pipe::ClientOptions::new()
                        .open(&path)
                        .map_err(|e| std::io::Error::from(e))?;

                    Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(client))
                }
            }
        }))
        .await?;

    Ok(channel)
}

/// Help to inject namespace into request.
///
/// To use this macro, the `tonic::Request` is needed.
#[macro_export]
macro_rules! with_namespace {
    ($req:expr, $ns:expr) => {{
        let mut req = Request::new($req);
        let md = req.metadata_mut();
        // https://github.com/containerd/containerd/blob/main/pkg/namespaces/grpc.go#L27
        md.insert("containerd-namespace", $ns.parse().unwrap());
        req
    }};
}

use services::v1::{
    containers_client::ContainersClient,
    content_client::ContentClient,
    diff_client::DiffClient,
    events_client::EventsClient,
    images_client::ImagesClient,
    introspection_client::IntrospectionClient,
    leases_client::LeasesClient,
    namespaces_client::NamespacesClient,
    sandbox::{controller_client::ControllerClient, store_client::StoreClient},
    snapshots::snapshots_client::SnapshotsClient,
    streaming_client::StreamingClient,
    tasks_client::TasksClient,
    transfer_client::TransferClient,
    version_client::VersionClient,
};
use tonic::transport::{Channel, Error};

/// Client to containerd's APIs.
pub struct Client {
    channel: Channel,
}

impl From<Channel> for Client {
    fn from(value: Channel) -> Self {
        Self { channel: value }
    }
}

impl Client {
    /// Create a new client from UDS socket.
    #[cfg(feature = "connect")]
    pub async fn from_path(path: impl AsRef<std::path::Path>) -> Result<Self, Error> {
        let channel = connect(path).await?;
        Ok(Self { channel })
    }

    /// Access to the underlying Tonic channel.
    #[inline]
    pub fn channel(&self) -> Channel {
        self.channel.clone()
    }

    /// Version service.
    #[inline]
    pub fn version(&self) -> VersionClient<Channel> {
        VersionClient::new(self.channel())
    }

    /// Task service client.
    #[inline]
    pub fn tasks(&self) -> TasksClient<Channel> {
        TasksClient::new(self.channel())
    }

    /// Transfer service client.
    #[inline]
    pub fn transfer(&self) -> TransferClient<Channel> {
        TransferClient::new(self.channel())
    }

    /// Sandbox store client.
    #[inline]
    pub fn sandbox_store(&self) -> StoreClient<Channel> {
        StoreClient::new(self.channel())
    }

    /// Streaming services client.
    #[inline]
    pub fn streaming(&self) -> StreamingClient<Channel> {
        StreamingClient::new(self.channel())
    }

    /// Sandbox controller client.
    #[inline]
    pub fn sandbox_controller(&self) -> ControllerClient<Channel> {
        ControllerClient::new(self.channel())
    }

    /// Snapshots service.
    #[inline]
    pub fn snapshots(&self) -> SnapshotsClient<Channel> {
        SnapshotsClient::new(self.channel())
    }

    /// Namespaces service.
    #[inline]
    pub fn namespaces(&self) -> NamespacesClient<Channel> {
        NamespacesClient::new(self.channel())
    }

    /// Leases service.
    #[inline]
    pub fn leases(&self) -> LeasesClient<Channel> {
        LeasesClient::new(self.channel())
    }

    /// Intropection service.
    #[inline]
    pub fn introspection(&self) -> IntrospectionClient<Channel> {
        IntrospectionClient::new(self.channel())
    }

    /// Image service.
    #[inline]
    pub fn images(&self) -> ImagesClient<Channel> {
        ImagesClient::new(self.channel())
    }

    /// Event service.
    #[inline]
    pub fn events(&self) -> EventsClient<Channel> {
        EventsClient::new(self.channel())
    }

    /// Diff service.
    #[inline]
    pub fn diff(&self) -> DiffClient<Channel> {
        DiffClient::new(self.channel())
    }

    /// Content service.
    #[inline]
    pub fn content(&self) -> ContentClient<Channel> {
        ContentClient::new(self.channel())
    }

    /// Container service.
    #[inline]
    pub fn containers(&self) -> ContainersClient<Channel> {
        ContainersClient::new(self.channel())
    }
}

#[cfg(test)]
mod tests {
    use prost_types::Any;

    use crate::events::ContainerCreate;

    #[test]
    fn any_roundtrip() {
        let original = ContainerCreate {
            id: "test".to_string(),
            image: "test".to_string(),
            runtime: None,
        };

        let any = Any::from_msg(&original).expect("should not fail to encode");
        let decoded: ContainerCreate = any.to_msg().expect("should not fail to decode");

        assert_eq!(original, decoded)
    }
}
