# Rust extensions for containerd

[![CI](https://github.com/mxpv/shim-rs/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/mxpv/shim-rs/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/l/containerd-client)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)
[![dependency status](https://deps.rs/repo/github/containerd/rust-extensions/status.svg)](https://deps.rs/repo/github/containerd/rust-extensions)

A collection of Rust crates to extend containerd.

This repository contains the following crates:

| Name | Description | Links |
| --- | --- | --- |
| [containerd-shim-protos](crates/shim-protos) | TTRPC bindings to shim interfaces | [![Crates.io](https://img.shields.io/crates/v/containerd-shim-protos)](https://crates.io/crates/containerd-shim-protos) [![docs.rs](https://img.shields.io/docsrs/containerd-shim-protos)](https://docs.rs/containerd-shim-protos/latest/containerd_shim_protos/) |
| [containerd-shim-logging](crates/logging) | Shim logger plugins | [![Crates.io](https://img.shields.io/crates/v/containerd-shim-logging)](https://crates.io/crates/containerd-shim-logging) [![docs.rs](https://img.shields.io/docsrs/containerd-shim-logging)](https://docs.rs/containerd-shim-logging/latest/containerd_shim_logging/) |
| [containerd-shim](crates/shim) | Runtime v2 shim wrapper | [![Crates.io](https://img.shields.io/crates/v/containerd-shim)](https://crates.io/crates/containerd-shim) [![docs.rs](https://img.shields.io/docsrs/containerd-shim)](https://docs.rs/containerd-shim/latest/containerd_shim/) |
| [containerd-client](crates/client) | GRPC bindings to containerd APIs | [![Crates.io](https://img.shields.io/crates/v/containerd-client)](https://crates.io/crates/containerd-client) [![docs.rs](https://img.shields.io/docsrs/containerd-client)](https://docs.rs/containerd-client/latest/containerd_client/) |
| [containerd-snapshots](crates/snapshots) | Remote snapshotter for containerd | [![Crates.io](https://img.shields.io/crates/v/containerd-snapshots)](https://crates.io/crates/containerd-snapshots) [![docs.rs](https://img.shields.io/docsrs/containerd-snapshots)](https://docs.rs/containerd-snapshots/latest/containerd_snapshots/) |
| [runc](crates/runc) | Rust wrapper for runc CLI | [![Crates.io](https://img.shields.io/crates/v/runc)](https://crates.io/crates/runc) [![docs.rs](https://img.shields.io/docsrs/runc)](https://docs.rs/runc/latest/runc/) |
| [containerd-runc-shim](crates/runc-shim) | Runtime v2 runc shim implementation | [![Crates.io](https://img.shields.io/crates/v/containerd-runc-shim)](https://crates.io/crates/containerd-runc-shim) [![docs.rs](https://img.shields.io/docsrs/containerd-runc-shim)](https://docs.rs/containerd-runc-shim/latest/containerd-runc-shim/) |


## How to build
The build process as easy as:
```bash
cargo build --release
```

## Minimum supported Rust version (MSRV)
The minimum supported version of Rust is `1.54`
