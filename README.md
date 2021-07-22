# Rust extensions for containerd

[![CI](https://github.com/mxpv/shim-rs/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/mxpv/shim-rs/actions/workflows/ci.yml)

Runtime related crates for containerd that let you implement v2 runtimes in Rust.

This repository contains the following crates:

| Name | Description |
| --- | --- |
| [containerd-protos](crates/protos) | Generated TTRPC bindings for containerd |
| [shim](crates/shim) | Runtime v2 shim wrapper |
| [containerd-shim-logging](crates/logging) | Shim logger |

## How to build
The build process as easy as:
```bash
git submodule update --init
cargo build --release
```

[containerd-protos](crates/protos) crate pulls [containerd](https://github.com/containerd/containerd) as a submodule to generate TTRPC bindings.
