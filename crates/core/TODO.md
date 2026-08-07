# TODO

## Resource path resolution: switch to `current_exe()` for deployed binaries

`config/default.yml`, `containers/*.yaml`, and bind-mount sources resolved in
`resolve_bind_source` are currently anchored with `env!("CARGO_MANIFEST_DIR")`
(see `src/configuration/mod.rs` and `src/docker/mod.rs`). This is a
compile-time constant — it bakes the build machine's source checkout path
(e.g. `/home/nino/repositories/core/crates/core`) directly into the binary.

This is enough to make `cargo run`/`cargo test` behave the same regardless of
invocation CWD, which was the immediate problem. It is **not** enough for a
binary that runs somewhere other than its build tree — a `cargo install`,
a CI-built release artifact, or (most relevant here, given this crate
orchestrates Docker) a slim runtime image that only `COPY`s the compiled
binary. In all of those cases the baked-in path won't exist, and resource
loading will fail again.

### The fix, when packaging/deployment is actually built

Resolve resource paths at **runtime**, relative to the running executable,
instead of at compile time relative to the source tree:

```rust
fn resource_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .expect("failed to get current executable path")
        .parent()
        .expect("executable has no parent directory")
        .to_path_buf()
}
```

Then anchor `config/default`, `containers/`, and bind-mount sources on
`resource_dir()` instead of `env!("CARGO_MANIFEST_DIR")`.

**Precondition:** this only works if `config/` and `containers/` are
physically placed next to the compiled binary at runtime, in *every* context
the binary runs in — plain `cargo run` puts the binary in `target/debug/` or
`target/release/`, not next to `crates/core/config/`, so a packaging step
(build script, `Dockerfile` `COPY`, release archive step, etc.) needs to
co-locate them before this works locally too. Do this alongside whatever
Docker packaging/deployment story gets built for this crate, not before.
