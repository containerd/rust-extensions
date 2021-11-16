# Rust extensions for containerd

[![CI](https://github.com/mxpv/shim-rs/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/mxpv/shim-rs/actions/workflows/ci.yml)

A collection of Rust crates to extend containerd.

This repository contains the following crates:

| Name | Description |
| --- | --- |
| [containerd-shim-client](crates/shim-client) | TTRPC bindings to shim interfaces |
| [containerd-shim-logging](crates/logging) | Shim logger plugins |
| [containerd-shim](crates/shim) | Runtime v2 shim wrapper 🚧 |
| [containerd-client](crates/client) | GRPC bindings to containerd APIs |

## How to build
The build process as easy as:
```bash
cargo build --release
```

## Minimum supported Rust version (MSRV)
The minimum supported version of Rust is `1.52`
