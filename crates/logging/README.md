# Shim logging binaries for containerd

[![Crates.io](https://img.shields.io/crates/v/containerd-shim-logging)](https://crates.io/crates/containerd-shim-logging)
[![docs.rs](https://img.shields.io/docsrs/containerd-shim-logging)](https://docs.rs/containerd-shim-logging/latest/containerd_shim_logging/)
[![Crates.io](https://img.shields.io/crates/l/containerd-shim-logging)](https://github.com/containerd/rust-extensions/blob/main/LICENSE)
[![CI](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/containerd/rust-extensions/actions/workflows/ci.yml)

Shim v2 runtime supports pluggable logging binaries via stdio URIs.
This crate implement `logging::run` to easy custom logger implementations in Rust.

[containerd Documentation](https://github.com/containerd/containerd/tree/master/runtime/v2#logging)

## Example

There is a journal example available as reference (originally written in Go [here](https://github.com/containerd/containerd/tree/dbef1d56d7ebc05bc4553d72c419ed5ce025b05d/runtime/v2#logging)):

```bash
# Build
$ sudo yum install systemd-devel
$ cargo build --example journal

# Run
$ ctr pull docker.io/library/hello-world:latest
$ ctr run --rm --log-uri=binary:////path/to/journal_binary docker.io/library/hello-world:latest hello
$ journalctl -f _COMM=journal
-- Logs begin at Thu 2021-05-20 15:47:51 PDT. --
Jul 22 11:53:35 dev journal[3233968]:
Jul 22 11:53:35 dev journal[3233968]: To try something more ambitious, you can run an Ubuntu container with:
Jul 22 11:53:35 dev journal[3233968]:  $ docker run -it ubuntu bash
Jul 22 11:53:35 dev journal[3233968]:
Jul 22 11:53:35 dev journal[3233968]: Share images, automate workflows, and more with a free Docker ID:
Jul 22 11:53:35 dev journal[3233968]:  https://hub.docker.com/
Jul 22 11:53:35 dev journal[3233968]:
Jul 22 11:53:35 dev journal[3233968]: For more examples and ideas, visit:
Jul 22 11:53:35 dev journal[3233968]:  https://docs.docker.com/get-started/
Jul 22 11:53:35 dev journal[3233968]:
```
