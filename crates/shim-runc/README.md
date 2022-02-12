# Rust made containerd-shim-runc-v2 

Rust Implementation for shim-runc-v2.
Go counterparts are in [containerd/runtime/v2/runc](https://github.com/containerd/containerd/tree/main/runtime/v2/runc).

## Usage
ctr: 
```shell
sudo ctr run -d --rm --runtime /path/to/shim docker.io/library/nginx:alpine <container-id>
```

## Limitations
- Supported tasks are only:
    - connect
    - shutdown
    - create
    - start
    - state
    - wait
    - kill
    - delete
- IO utilities are **partially** supported now.
    - Thus, we cannot use terminal
        - i.e. detach new-terminal mode is not available: see Runc's [terminals.md](https://github.com/opencontainers/runc/blob/f99d252d2bf7416599b8a87f9d91b3c0f96bf593/docs/terminals.md) 
        - This will be supported after console implementation in runc crate completes.
    - However, there are skeleton codes in `src/process`
        - `io.rs` and `fifo.rs`
        - Go couterparts are [process](https://github.com/containerd/containerd/tree/main/pkg/process) and [fifo](https://github.com/containerd/fifo).

