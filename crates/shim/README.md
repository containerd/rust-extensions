# containerd shim

Rust crate to ease runtime v2 shim implementation.

It replicates same [shim.Run](https://github.com/containerd/containerd/blob/dbef1d56d7ebc05bc4553d72c419ed5ce025b05d/runtime/v2/example/cmd/main.go)
API offered by containerd's shim v2 runtime implementation written in Go.

## Look and feel

The API is very similar to the one offered by Go version:

```rust
struct Service {
    exit: shim::ExitSignal,
}

impl shim::Shim for Service {
    fn new(
        _id: &str,
        _namespace: &str,
        _publisher: shim::RemotePublisher,
        _config: &mut shim::Config,
        exit: shim::ExitSignal,
    ) -> Self {
        Service { exit }
    }

    fn start_shim(&mut self, _opts: shim::StartOpts) -> Result<String, Box<dyn Error>> {
        Ok("Socket address here".into())
    }
}

impl shim::Task for Service {
    fn create(
        &self,
        _ctx: &TtrpcContext,
        _req: api::CreateTaskRequest,
    ) -> TtrpcResult<api::CreateTaskResponse> {
        // New task nere...
        Ok(api::CreateTaskResponse::default())
    }

    fn shutdown(&self, _ctx: &TtrpcContext, _req: api::ShutdownRequest) -> TtrpcResult<api::Empty> {
        self.exit.signal(); // Signal to shutdown shim server
        Ok(api::Empty::default())
    }
}

fn main() {
    shim::run::<Service>("io.containerd.empty.v1")
}

```

## How to use

Runtime binary has to be [named in a special way](https://github.com/containerd/containerd/blob/main/runtime/v2/README.md#binary-naming) to be recognized by containerd:

```bash
$ cargo build --example empty-shim
$ sudo cp ./target/debug/examples/empty-shim /usr/local/bin/containerd-shim-empty-v2
$ sudo ctr run --rm --runtime io.containerd.empty.v2 -t docker.io/library/hello-world:latest hello
```
