use std::convert::TryFrom;
use tokio::net::UnixStream;

use containerd_client_protos as protos;
use protos::services::v1::version_client::VersionClient;
use protos::tonic::transport::Endpoint;

/// Make sure you run containerd before running this example.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    // Taken from https://github.com/hyperium/tonic/commit/b90c3408001f762a32409f7e2cf688ebae39d89e#diff-f27114adeedf7b42e8656c8a86205685a54bae7a7929b895ab62516bdf9ff252R15
    let channel = Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(|_| {
            UnixStream::connect("/run/containerd/containerd.sock")
        }))
        .await
        .expect("Connect Failed");

    let mut client = VersionClient::new(channel);
    let resp = client.version(()).await.expect("Failed to query version");

    println!("Response: {:?}", resp.get_ref());
}
