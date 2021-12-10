# Rust extensions for containerd

[![CI](https://github.com/mxpv/shim-rs/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/mxpv/shim-rs/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/l/containerd-client)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)

A collection of Rust crates to extend containerd.

This repository contains the following crates:

| Name | Description | Links                                                                                                                    |
| --- | --- |--------------------------------------------------------------------------------------------------------------------------|
| [containerd-shim-client](crates/shim-client) | TTRPC bindings to shim interfaces | [![Crates.io](https://img.shields.io/crates/v/containerd-shim-client)](https://crates.io/crates/containerd-shim-client)  |
| [containerd-shim-logging](crates/logging) | Shim logger plugins | [![Crates.io](https://img.shields.io/crates/v/containerd-shim-logging)](https://crates.io/crates/containerd-shim-logging) |
| [containerd-shim](crates/shim) | Runtime v2 shim wrapper | [![Crates.io](https://img.shields.io/crates/v/containerd-shim)](https://crates.io/crates/containerd-shim)                |
| [containerd-client](crates/client) | GRPC bindings to containerd APIs | [![Crates.io](https://img.shields.io/crates/v/containerd-client)](https://crates.io/crates/containerd-client) |

## How to build
The build process as easy as:
```bash
cargo build --release
```

## Minimum supported Rust version (MSRV)
The minimum supported version of Rust is `1.52`
