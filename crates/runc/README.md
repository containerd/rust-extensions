# Rust bindings for runc CLI

[![Crates.io](https://img.shields.io/crates/v/runc)](https://crates.io/crates/runc)
[![docs.rs](https://img.shields.io/docsrs/runc)](https://docs.rs/runc/latest/runc/)
[![Crates.io](https://img.shields.io/crates/l/containerd-shim)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)
[![CI](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml)

A crate for consuming the runc binary in your Rust applications, similar to [go-runc](https://github.com/containerd/go-runc) for Go. 
This crate is based on archived [rust-runc](https://github.com/pwFoo/rust-runc).

## Usage
Both sync/async version is available.
You can build runc client with `RuncConfig` in method chaining style.
Call `build()` or `build_async()` to get client.
Note that async client depends on [tokio](https://github.com/tokio-rs/tokio), then please use it on tokio runtime.

```rust
use runc;

#[tokio::main]
async fn main() {
    let config = runc::Config::new()
        .root("./new_root")
        .debug(false)
        .log("/path/to/logfile.json")
        .log_format(runc::LogFormat::Json)
        .rootless(true);

    let client = config.build_async().unwrap();

    let opts = runc::options::CreateOpts::new()
        .pid_file("/path/to/pid/file")
        .no_pivot(true);
    
    client.create("container-id", "path/to/bundle", Some(&opts)).unwrap();
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