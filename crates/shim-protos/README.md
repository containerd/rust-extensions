# Shim protos and client for containerd

[![Crates.io](https://img.shields.io/crates/v/containerd-shim-protos)](https://crates.io/crates/containerd-shim-protos)
[![docs.rs](https://img.shields.io/docsrs/containerd-shim-protos)](https://docs.rs/containerd-shim-protos/latest/containerd_shim_protos/)
[![Crates.io](https://img.shields.io/crates/l/containerd-shim-protos)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)
[![CI](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml)

`containerd-shim-protos` contains TTRPC bindings and client/server code to interact with containerd's runtime v2 shims.

## Runtime
This crate is mainly expected to be useful to interact with containerd's shim runtime.
Runtime v2 introduces a first class shim API for runtime authors to integrate with containerd.
The shim API is minimal and scoped to the execution lifecycle of a container.

To learn how containerd's shim v2 runtime works in details, please refer to the [documentation](https://github.com/containerd/containerd/blob/main/core/runtime/v2/README.md).

## Design
The `containerd-shim-protos` crate provides [Protobuf](https://github.com/protocolbuffers/protobuf.git) message
and [TTRPC](https://github.com/containerd/ttrpc.git) service definitions for the
[Containerd shim v2](https://github.com/containerd/containerd/blob/main/api/runtime/task/v2/shim.proto) protocol.

The message and service definitions are auto-generated from protobuf source files under `vendor/`
by using [ttrpc-codegen](https://github.com/containerd/ttrpc-rust/tree/master/ttrpc-codegen). So please do not
edit those auto-generated source files.

If upgrading/modification is needed, please follow the steps:
 - Synchronize the latest protobuf source files from the upstream projects into directory 'vendor/'.
 - Re-generate the source files by `cargo build --features=generate_bindings`.
 - Commit the synchronized protobuf source files and auto-generated source files, keeping them in synchronization.

## Usage
Add `containerd-shim-client` as a dependency in your `Cargo.toml`

```toml
[dependencies]
containerd-shim-protos = "0.4"
```

Basic client code looks as follows:

```rust,no_run
use containerd_shim_protos as client;

let client = client::Client::connect("unix:///containerd-shim/shim.sock").expect("Failed to connect to shim");
let task_client = client::TaskClient::new(client);

let context = client::ttrpc::context::with_timeout(0);

let req = client::api::ConnectRequest {
    id: String::from("1"),
    ..Default::default()
};

let resp = task_client.connect(context, &req).expect("Connect request failed");
```

## Example

- [TTRPC shim client](./examples/ttrpc-client.rs)
- [TTRPC shim server](./examples/ttrpc-server.rs)
- [TTRPC client connect](./examples/connect.rs).

The way to build the example:
```bash
# build sync connect, client and server
$ cargo build --example shim-proto-connect
$ sudo ./shim-proto-connect unix:///containerd-shim/shim_socket_path.sock
$ cargo build --example shim-proto-client
$ cargo build --example shim-proto-server

# build async connect, client and server
$ cargo build --example shim-proto-connect-async --features async
$ cargo build --example shim-proto-client-async --features async
$ cargo build --example shim-proto-server-async --features async
```
