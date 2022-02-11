# Remote snapshotter extension for containerd

[![Crates.io](https://img.shields.io/crates/v/containerd-snapshots)](https://crates.io/crates/containerd-snapshots)
[![docs.rs](https://img.shields.io/docsrs/containerd-snapshots)](https://docs.rs/containerd-snapshots/latest/containerd_snapshots/)
[![Crates.io](https://img.shields.io/crates/l/containerd-shim-logging)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)
[![CI](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml)

Snapshots crate implements containerd's proxy plugin for snapshotting. It aims hide the underlying complexity of GRPC
interfaces, streaming, and request/response conversions and provide one `Snapshots` trait to implement.

[containerd Documentation](https://github.com/containerd/containerd/blob/main/docs/PLUGINS.md#proxy-plugins)

## Proxy plugins

A proxy plugin is configured using containerd's config file and will be loaded alongside the internal plugins when
containerd is started. These plugins are connected to containerd using a local socket serving one of containerd's GRPC
API services. Each plugin is configured with a type and name just as internal plugins are.

## How to use from containerd

Add the following to containerd's configuration file:
```toml
[proxy_plugins]
  [proxy_plugins.custom]
    type = "snapshot"
    address = "/tmp/snap2.sock"
```

Start daemons and try pulling an image with `custom` snapshotter:
```bash
# Start containerd daemon
$ containerd --config /path/config.toml

# Run remote snapshotter instance
$ cargo run --example snapshotter /tmp/snap2.sock

# Now specify the snapshotter when pulling an image
$ ctr i pull --snapshotter custom docker.io/library/hello-world:latest
```

## Getting started

Snapshotters are required to implement `Snapshotter` trait (which is very similar to containerd's
[Snapshotter](https://github.com/containerd/containerd/blob/main/snapshots/snapshotter.go) interface).

```rust
#[derive(Default)]
struct Example;

#[snapshots::tonic::async_trait]
impl snapshots::Snapshotter for Example {
    type Error = ();

    async fn stat(&self, key: String) -> Result<Info, Self::Error> {
        info!("Stat: {}", key);
        Ok(Info::default())
    }

    // ...

    async fn commit(
        &self,
        name: String,
        key: String,
        labels: HashMap<String, String>,
    ) -> Result<(), Self::Error> {
        info!("Commit: name={}, key={}, labels={:?}", name, key, labels);
        Ok(())
    }
}
```

The library provides `snapshots::server` for convenience to wrap the implementation into a GRPC server, so it can
be used with `tonic` like this:

```rust

use snapshots::tonic::transport::Server;

Server::builder()
    .add_service(snapshots::server(example))
    .serve_with_incoming(incoming)
    .await
    .expect("Serve failed");
```