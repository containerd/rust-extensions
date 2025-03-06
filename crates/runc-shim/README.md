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

## Performance test

### Memory overhead

Three different kinds of shim binaries are used to compare memory overhead, first is `containerd-shimv2-runc-v2`
compiled by golang, next is our sync `containerd-shim-runc-v2-rs` and the last one is our async `containerd-shim-runc-v2-rs`
but limited to 2 work threads.

We run a *busybox* container inside a pod on a *16U32G Ubuntu20.04* machine with *containerd v1.6.8* and *runc v1.1.4*.
To measure the memory size of shim process we parse the output of *smaps* file and add up all RSS segments.
In addition, we also run 100 pods and collect the total memory overhead.

 |                                                              | Single Process RSS | 100 Processes RSS |
 | :----------------------------------------------------------- | :----------------- | :---------------- |
 | containerd-shim-runc-v2                                      | 11.02MB            | 1106.52MB         |
 | containerd-shim-runc-v2-rs(sync)                             | 3.45MB             | 345.39MB          |
 | containerd-shim-runc-v2-rs(async, limited to 2 work threads) | 3.90MB             | 396.83MB          |
