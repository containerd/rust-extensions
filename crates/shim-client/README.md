# Shim protos

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
