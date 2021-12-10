# Shim protos and client for containerd

[![Crates.io](https://img.shields.io/crates/v/containerd-shim-client)](https://crates.io/crates/containerd-shim-client)
[![docs.rs](https://img.shields.io/docsrs/containerd-shim-client)](https://docs.rs/containerd-shim-client/latest/containerd-shim-client/)
[![Crates.io](https://img.shields.io/crates/l/containerd-shim-client)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)
[![CI](https://github.com/mxpv/shim-rs/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/mxpv/shim-rs/actions/workflows/ci.yml)

TTRPC bindings for containerd's shim events and interfaces.

## Look and feel

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

Have a look on example [here](./examples/connect.rs).

```bash
$ cargo build --example connect
$ sudo ./connect unix:///containerd-shim/shim_socket_path.sock
```
