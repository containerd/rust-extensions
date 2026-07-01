# Rust bindings for runc CLI

[![Crates.io](https://img.shields.io/crates/v/runc)](https://crates.io/crates/runc)
[![docs.rs](https://img.shields.io/docsrs/runc)](https://docs.rs/runc/latest/runc/)
[![Crates.io](https://img.shields.io/crates/l/containerd-shim)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)
[![CI](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml)

A crate for consuming the runc binary in your Rust applications, similar to [go-runc](https://github.com/containerd/go-runc) for Go.
This crate is based on archived [rust-runc](https://github.com/pwFoo/rust-runc).

## Usage
Both sync and async versions are available.
You can build a runc client with `GlobalOpts` in method chaining style.
Call `build()` to get a client.
To use the async client, enable the `async` feature and run it on a [tokio](https://github.com/tokio-rs/tokio) runtime.

```rust,ignore
#[tokio::main]
async fn main() {
    let config = runc::options::GlobalOpts::new()
        .root("./new_root")
        .debug(false)
        .log("/path/to/logfile.json")
        .log_format(runc::LogFormat::Json)
        .rootless(true);

    let client = config.build().unwrap();

    let opts = runc::options::CreateOpts::new()
        .pid_file("/path/to/pid/file")
        .no_pivot(true);

    client
        .create("container-id", "path/to/bundle", Some(&opts))
        .await
        .unwrap();
}
```

## Limitations
- Supported commands are only:
    - create
    - start
    - state
    - kill
    - delete
- Exec is **not** available in `RuncAsyncClient` now.
- Console utilites are **not** available
    - see [Go version](https://github.com/containerd/go-runc/blob/main/console.go)
