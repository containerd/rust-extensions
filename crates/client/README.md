# containerd GRPC client

[![Crates.io](https://img.shields.io/crates/v/containerd-client)](https://crates.io/crates/containerd-client)
[![docs.rs](https://img.shields.io/docsrs/containerd-client)](https://docs.rs/containerd-client/latest/containerd_client/)
[![Crates.io](https://img.shields.io/crates/l/containerd-client)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)
[![CI](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml)

This crate implements a GRPC client to query containerd APIs.

## Example

```rust
// Launch containerd at /run/containerd/containerd.sock
let channel = connect("/run/containerd/containerd.sock").await?;

let mut client = VersionClient::new(channel);
let resp = client.version(()).await?;

println!("Response: {:?}", resp.get_ref());
```
