# shim-instrument

Provides a way to conditionally compile `tracing` instrumentation into the code based on a feature flag "tracing". This can be useful in scenarios where you want to avoid the overhead of tracing in production or in builds where tracing is not required.

## Usage

In `Cargo.toml`:

```toml
[dependencies]
shim-instrument = { path = "../../shim-instrument" }
tracing = { version = "0.1", optional = true }

[features]
tracing = ["dep:tracing", "shim-instrument/tracing"]
```

In code:

```rust
#[shim_instrument(skip_all, level = "Info")]
fn f1(a: i32) -> i32 {
    a + 1
}
```

To enable tracing, build with the `tracing` feature:

```sh
cargo build --features tracing
```