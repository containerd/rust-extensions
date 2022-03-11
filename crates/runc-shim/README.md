# Rust containerd shim v2 for runc container

[![Crates.io](https://img.shields.io/crates/v/containerd-runc-shim)](https://crates.io/crates/containerd-runc-shim)
[![docs.rs](https://img.shields.io/docsrs/containerd-runc-shim)](https://docs.rs/containerd-runc-shim/latest/containerd-runc-shim/)
[![Crates.io](https://img.shields.io/crates/l/containerd-shim)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)
[![CI](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml)

By default [containerd](https://github.com/containerd/containerd) relies on runc shim v2 runtime (written in `Go`) to launch containers.
This crate is an alternative Rust implementation of the shim runtime.
It conforms to containerd's integration tests and can be replaced with the original Go runtime interchangeably.

## Usage

To build binary, run:
```shell
cargo build --release --bin containerd-shim-runc-v2-rs
```

Replace it to the containerd shim dir: `/usr/local/bin/containerd-shim-runc-v2-rs`

In order to use it from containerd, use:

```shell
$ sudo ctr run --rm --runtime io.containerd.runc.v2-rs -t docker.io/library/hello-world:latest hello
```

You can run a container by `ctr`, `crictl` or kubernetes API.
