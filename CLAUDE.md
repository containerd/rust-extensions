# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

Cargo workspace of Rust crates for extending containerd (edition 2024, toolchain/MSRV pinned in `rust-toolchain.toml`):

- `shim-protos` — TTRPC bindings for the shim APIs (generated from vendored `.proto` files)
- `shim` — runtime v2 shim framework (`Shim` trait, bootstrap/start/delete protocol, publisher, reaper, monitor)
- `runc` — wrapper around the runc CLI
- `runc-shim` — the runc v2 shim binary, built as `containerd-shim-runc-v2-rs` (registered in containerd as `io.containerd.runc.v2-rs`)
- `client` — tonic gRPC bindings to containerd's APIs
- `snapshots` — remote snapshotter gRPC service
- `logging` — shim logger plugin helpers

## Build, lint, test

`protoc` must be installed: `build.rs` in `shim-protos`, `client` and `snapshots` generates code from protos (`scripts/install-protobuf.sh` installs it in CI).

```bash
cargo build --release
cargo +nightly fmt --all          # rustfmt.toml uses unstable options; stable fmt won't apply them
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p containerd-runc-shim <test_name>   # single test
cargo test -p containerd-shim --features async
RUSTDOCFLAGS="--cfg docsrs -Dwarnings" cargo +nightly doc --no-deps
```

Package names differ from directory names: `containerd-shim`, `containerd-shim-protos`, `containerd-client`, `containerd-snapshots`, `containerd-shim-logging`, `containerd-runc-shim`, `runc`.

### Sync vs async halves

`runc` and `containerd-shim` each have a `synchronous` and an `asynchronous` module, selected by the `async` feature. They are **mutually exclusive** (`#[cfg(not(feature = "async"))]` gates the sync code), so `--all-features` never compiles the sync half, and because `runc-shim` enables `async` on both, a plain workspace build doesn't either. Check/clippy/test those crates on their own to cover the sync code, as CI does:

```bash
cargo clippy -p runc --all-targets -- -D warnings
cargo clippy -p containerd-shim --all-targets -- -D warnings
cargo test -p containerd-shim
```

Shared logic (e.g. runc io drivers) lives outside those two modules; changes there must compile under both feature sets.

### Platforms

- CI checks Linux gnu, Linux musl, and macOS. Code that is Linux-only (cgroups, mounts, eventfd, chown, etc.) must be `#[cfg(target_os = "linux")]`-gated, and tests that depend on it too.
- Windows only builds `containerd-shim`, `containerd-shim-protos` and `containerd-client`.
- Some tests need root (e.g. `runc-shim` cgroup tests) and `runc` tests need `XDG_RUNTIME_DIR` set; CI runs `cargo test` via `sudo env HOME=... PATH=... XDG_RUNTIME_DIR=...`.
- containerd integration tests (containerd's Go suite run against `containerd-shim-runc-v2-rs`) run only in CI (`linux-integration` job in `.github/workflows/ci.yml`), with a list of skipped tests there.

Other CI gates: `cargo deny` (`deny.toml`) and `cargo-machete` for unused dependencies — add a `[package.metadata.cargo-machete] ignored` entry when a dependency is used only indirectly.

## Protos

Proto files are vendored under each crate's `vendor/` directory. Don't edit them by hand: `scripts/update-vendor.sh` pulls a containerd release (version set in the script) and rsyncs the files listed in each crate's `rsync.txt`. Each crate's `build.rs` lists which protos get compiled; some modules are behind features (e.g. `sandbox` in `shim-protos`).

## Architecture notes

- `containerd-shim` re-exports the sync or async API at the crate root depending on the `async` feature (`cfg_async!` / `cfg_not_async!` macros in `lib.rs`). A shim implements the `Shim` trait and is started with `run::<T>(id, opts)`, which handles containerd's start/delete/bootstrap actions, the TTRPC server, event publishing back to containerd, and process reaping.
- `runc-shim` is async only. `service.rs` implements `Shim` (start/delete/wait, and the exit-event loop `process_exits`); `task.rs` implements the TTRPC `Task` service; `container.rs` holds a container's init and exec processes; `processes.rs` defines the generic `ProcessTemplate<S>` with `ProcessLifecycle` impls in `runc.rs` (`InitProcess` / `ExecProcess`) that call into the `runc` crate. Console/pty handling is in `console.rs`; cgroup memory events in `cgroup_memory.rs`.
- `rustfmt.toml` groups imports as std / external / crate and merges imports per crate; keep that layout when adding `use` statements.
