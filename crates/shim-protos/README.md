# Shim protos and client for containerd

[![Crates.io](https://img.shields.io/crates/v/containerd-shim-client)](https://crates.io/crates/containerd-shim-client)
[![docs.rs](https://img.shields.io/docsrs/containerd-shim-client)](https://docs.rs/containerd-shim-client/latest/containerd-shim-client/)
[![Crates.io](https://img.shields.io/crates/l/containerd-shim-client)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)
[![CI](https://github.com/mxpv/shim-rs/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/mxpv/shim-rs/actions/workflows/ci.yml)

TTRPC bindings for containerd's shim events and interfaces.

## Design

The `containerd-shim-protos` crate provides [Protobuf](https://github.com/protocolbuffers/protobuf.git) message
and [TTRPC](https://github.com/containerd/ttrpc.git) service definitions for the
[Containerd shim v2](https://github.com/containerd/containerd/blob/main/runtime/v2/task/shim.proto) protocol.

The message and service definitions are auto-generated from protobuf source files under `vendor/`
by using [ttrpc-codegen](https://github.com/containerd/ttrpc-rust/tree/master/ttrpc-codegen). So please do not
edit those auto-generated source files. If upgrading/modification is needed, please follow the steps:
- Synchronize the latest protobuf source files from the upstream projects into directory 'vendor/'.
- Re-generate the source files by `cargo build --features=generate_bindings`.
- Commit the synchronized protobuf source files and auto-generated source files, keeping them in synchronization.

## Usage
Add `containerd-shim-client` as a dependency in your `Cargo.toml`

```toml
[dependencies]
containerd-shim-protos = "0.1"
```

Basic client code looks as follows:

```rust
let client = client::Client::connect(socket_path)?;
let task_client = client::TaskClient::new(client);

let context = client::ttrpc::context::with_timeout(0);

let req = client::api::ConnectRequest {
    id: pid,
    ..Default::default()
};

let resp = task_client.connect(context, &req)?;
```

## Example

- [TTRPC shim client](./examples/ttrpc-client.rs)
- [TTRPC shim server](./examples/ttrpc-server.rs)
- [TTRPC client connect](./examples/connect.rs).

The way to build the [ttRPC client connect](./examples/connect.rs) example:
```bash
$ cargo build --example connect
$ sudo ./connect unix:///containerd-shim/shim_socket_path.sock
```
