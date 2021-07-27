# GRPC bindings to containerd APIs

This crate implements a GRPC client to query containerd APIs.

## Example

```rust
// Launch containerd at /run/containerd/containerd.sock
let channel = connect("/run/containerd/containerd.sock").await?;

let mut client = VersionClient::new(channel);
let resp = client.version(()).await?;

println!("Response: {:?}", resp.get_ref());
```
