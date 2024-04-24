# containerd GRPC client

[![Crates.io](https://img.shields.io/crates/v/containerd-client)](https://crates.io/crates/containerd-client)
[![docs.rs](https://img.shields.io/docsrs/containerd-client)](https://docs.rs/containerd-client/latest/containerd_client/)
[![Crates.io](https://img.shields.io/crates/l/containerd-client)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)
[![CI](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml)

This crate implements a GRPC client to query containerd APIs.

## Example

Run with `cargo run --example version`

```rust
use containerd_client::{connect, services::v1::version_client::VersionClient};

async fn query_version() {
    // Launch containerd at /run/containerd/containerd.sock
    let channel = connect("/run/containerd/containerd.sock").await.unwrap();

    let mut client = VersionClient::new(channel);
    let resp = client.version(()).await.unwrap();

    println!("Response: {:?}", resp.get_ref());
}
```
