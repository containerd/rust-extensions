# Shim extension for containerd

[![Crates.io](https://img.shields.io/crates/v/containerd-shim)](https://crates.io/crates/containerd-shim)
[![docs.rs](https://img.shields.io/docsrs/containerd-shim)](https://docs.rs/containerd-shim/latest/containerd_shim/)
[![Crates.io](https://img.shields.io/crates/l/containerd-shim)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)
[![CI](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml)

Rust crate to ease runtime v2 shim implementation.

It replicates same [shim.Run](https://github.com/containerd/containerd/blob/dbef1d56d7ebc05bc4553d72c419ed5ce025b05d/runtime/v2/example/cmd/main.go)
API offered by containerd's shim v2 runtime implementation written in Go.

## Look and feel

The API is very similar to the one offered by Go version:

```rust
#[derive(Clone)]
struct Service {
    exit: ExitSignal,
}

impl shim::Shim for Service {
    type Error = shim::Error;
    type T = Service;

    fn new(
        _runtime_id: &str,
        _id: &str,
        _namespace: &str,
        _publisher: shim::RemotePublisher,
        _config: &mut shim::Config,
    ) -> Self {
        Service {
            exit: ExitSignal::default(),
        }
    }

    fn start_shim(&mut self, opts: shim::StartOpts) -> Result<String, shim::Error> {
        let address = shim::spawn(opts, Vec::new())?;
        Ok(address)
    }

    fn wait(&mut self) {
        self.exit.wait();
    }

    fn get_task_service(&self) -> Self::T {
        self.clone()
    }
}

impl shim::Task for Service {
    fn connect(
        &self,
        _ctx: &TtrpcContext,
        _req: api::ConnectRequest,
    ) -> TtrpcResult<api::ConnectResponse> {
        info!("Connect request");
        Ok(api::ConnectResponse {
            version: String::from("example"),
            ..Default::default()
        })
    }

    fn shutdown(&self, _ctx: &TtrpcContext, _req: api::ShutdownRequest) -> TtrpcResult<api::Empty> {
        info!("Shutdown request");
        self.exit.signal(); // Signal to shutdown shim server
        Ok(api::Empty::default())
    }
}

fn main() {
    shim::run::<Service>("io.containerd.empty.v1")
}

```

## How to use with containerd

**Note**: All operations are in the root directory of `rust-extensions`.

With shim v2 runtime:

```bash
$ cargo build --example skeleton
$ sudo cp ./target/debug/examples/skeleton /usr/local/bin/containerd-shim-skeleton-v1
$ sudo ctr run --rm --runtime io.containerd.skeleton.v1 -t docker.io/library/hello-world:latest hello
```

Or if on 1.6+

```bash
$ cargo build --example skeleton
$ sudo ctr run --rm --runtime ./target/debug/examples/skeleton docker.io/library/hello-world:latest hello
```

Or manually:

```
$ touch log

# Run containerd in background
$ sudo TTRPC_ADDRESS="/var/run/containerd/containerd.sock.ttrpc" \
    cargo run --example skeleton -- \
    -namespace default \
    -id 1234 \
    -address /var/run/containerd/containerd.sock \
    -publish-binary ./bin/containerd \
    start
unix:///var/run/containerd/eb8e7d1c48c2a1ec.sock

$ cargo build --example shim-proto-connect
$ sudo ./target/debug/examples/shim-proto-connect unix:///var/run/containerd/eb8e7d1c48c2a1ec.sock
Connecting to unix:///var/run/containerd/eb8e7d1c48c2a1ec.sock...
Sending `Connect` request...
Connect response: version: "example"
Sending `Shutdown` request...
Shutdown response: ""

$ cat log
[INFO] server listen started
[INFO] server started
[INFO] Shim successfully started, waiting for exit signal...
[INFO] Connect request
[INFO] Shutdown request
[INFO] Shutting down shim instance
[INFO] close monitor
[INFO] listener shutdown for quit flag
[INFO] ttrpc server listener stopped
[INFO] listener thread stopped
[INFO] begin to shutdown connection
[INFO] connections closed
[INFO] reaper thread exited
[INFO] reaper thread stopped
```

## Supported Platforms
Currently, following OSs and hardware architectures are supported, and more efforts are needed to enable and validate other OSs and architectures.
- Linux
- Mac OS
