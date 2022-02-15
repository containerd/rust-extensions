# Rust containerd shim v2 for runc container

This is a binary crate that provides a containerd shim for runc container.

## Usage

To build binary, run:
```shell
cargo build --release
```

Replace it to the containerd shim dir: `/usr/local/bin/containerd-shim-runc-v2`

You can run a container by `ctr`, `crictl` or kubernetes API.