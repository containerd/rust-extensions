# containerd shim

Rust crate to ease runtime v2 shim implementation.

It replicates same [shim.Run](https://github.com/containerd/containerd/blob/dbef1d56d7ebc05bc4553d72c419ed5ce025b05d/runtime/v2/example/cmd/main.go)
API offered by containerd's shim v2 runtime implementation written in Go.

## Look and feel

The API is very similar to the one offered by Go version:

```rust
struct Service;

impl shim::Shim for Service {
    fn new(_id: &str, _namespace: &str, _config: &mut shim::Config) -> Self {
        Service {}
    }

    fn start_shim(&mut self, opts: StartOpts) -> Result<String, Box<dyn Error>> {
        let address = shim::spawn(opts)?;
        Ok(address)
    }

    fn delete_shim(&mut self) -> Result<api::DeleteResponse, Box<dyn Error>> {
        todo!()
    }
}

impl shim::Task for Service {
    fn create(
        &self,
        ctx: &TtrpcContext,
        req: api::CreateTaskRequest,
    ) -> ::ttrpc::Result<api::CreateTaskResponse> {
        debug!("Create");
        Ok(api::CreateTaskResponse::default())
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
