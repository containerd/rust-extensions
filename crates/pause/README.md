# containerd-pause

[![Crates.io](https://img.shields.io/crates/v/containerd-pause)](https://crates.io/crates/containerd-pause)
[![Crates.io](https://img.shields.io/crates/l/containerd-pause)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)
[![CI](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml)

A minimal process that holds a Linux PID namespace open.

The kernel destroys a PID namespace, and kills everything in it, as soon as
its PID 1 exits. Unlike other namespaces, it cannot be kept alive with a bind
mount. `containerd-pause` runs as PID 1 of the namespace and does nothing else:

- it ignores `SIGINT` and `SIGTERM`, so only `SIGKILL` stops it;
- it sets `SA_NOCLDWAIT` on `SIGCHLD`, so the kernel reaps processes
  reparented to it;
- it sleeps in `pause(2)` until killed.

Release builds are `no_std` and link only libc.

## Usage

```shell
cargo build --release --bin containerd-pause
```

Spawn the binary with `CLONE_NEWPID` (for example
`unshare --pid --fork containerd-pause`) and send it `SIGKILL` to tear the
namespace down.
