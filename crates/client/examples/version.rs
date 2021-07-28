use containerd_client as client;

use client::services::v1::version_client::VersionClient;

/// Make sure you run containerd before running this example.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let channel = client::connect("/run/containerd/containerd.sock")
        .await
        .expect("Connect Failed");

    let mut client = VersionClient::new(channel);
    let resp = client.version(()).await.expect("Failed to query version");

    println!("Response: {:?}", resp.get_ref());
}
