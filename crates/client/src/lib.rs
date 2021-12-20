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

//! A GRPC client to query containerd's API.

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
    pub mod v1 {
        tonic::include_proto!("containerd.services.containers.v1");
        tonic::include_proto!("containerd.services.content.v1");
        tonic::include_proto!("containerd.services.diff.v1");
        tonic::include_proto!("containerd.services.events.v1");
        tonic::include_proto!("containerd.services.images.v1");
        tonic::include_proto!("containerd.services.introspection.v1");
        tonic::include_proto!("containerd.services.leases.v1");
        tonic::include_proto!("containerd.services.namespaces.v1");
        tonic::include_proto!("containerd.services.tasks.v1");

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
/// This helper inteded to be used in conjuction with [Tokio](https://tokio.rs) runtime.
#[cfg(feature = "connect")]
pub async fn connect(
    path: impl AsRef<std::path::Path>,
) -> Result<tonic::transport::Channel, tonic::transport::Error> {
    use std::convert::TryFrom;
    use tokio::net::UnixStream;
    use tonic::transport::Endpoint;

    let path = path.as_ref().to_path_buf();

    // Taken from https://github.com/hyperium/tonic/commit/b90c3408001f762a32409f7e2cf688ebae39d89e#diff-f27114adeedf7b42e8656c8a86205685a54bae7a7929b895ab62516bdf9ff252R15
    let channel = Endpoint::try_from("https://[::]")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_| {
            UnixStream::connect(path.clone())
        }))
        .await?;

    Ok(channel)
}

/// Help to inject namespace into request.
///
/// To use this macro, the `tonic::Request` is needed.
#[macro_export]
macro_rules! with_namespace {
    ($req : ident, $ns: expr) => {{
        let mut req = Request::new($req);
        let md = req.metadata_mut();
        // https://github.com/containerd/containerd/blob/main/namespaces/grpc.go#L27
        md.insert("containerd-namespace", $ns.parse().unwrap());
        req
    }};
}
