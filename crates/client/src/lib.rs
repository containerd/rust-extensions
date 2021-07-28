pub use tonic;

pub mod types {
    tonic::include_proto!("containerd.types");

    pub mod v1 {
        tonic::include_proto!("containerd.v1.types");
    }
}

pub mod google {
    pub mod rpc {
        tonic::include_proto!("google.rpc");
    }
}

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

        // Snapshot's `Info` conflicts with Content's `Info`, so wrap it into a separate sub module.
        pub mod snapshots {
            tonic::include_proto!("containerd.services.snapshots.v1");
        }

        tonic::include_proto!("containerd.services.version.v1");
    }
}

pub mod events {
    tonic::include_proto!("containerd.events");
}

/// Connect creates a unix channel to containerd GRPC socket.
/// This helper inteded to be used in conjuction with Tokio runtime.
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
