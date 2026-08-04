# Directories and File Naming

## Workspace layout

| Path | Contents |
|------|----------|
| `Cargo.toml` | Workspace root manifest. Defines `members = ["crates/*"]` and `[workspace.dependencies]`. |
| `rust-toolchain.toml` | Pins the toolchain channel for every contributor. |
| `rustfmt.toml` / `clippy.toml` / `deny.toml` | Tooling configuration. |
| `crates/<name>/` | One directory per crate. |
| `target/` | Build output. Git-ignored. |

## Per-crate layout

| Path | Contents |
|------|----------|
| `crates/<name>/Cargo.toml` | Crate manifest. Inherits shared keys from the workspace via `<key>.workspace = true`. |
| `crates/<name>/src/lib.rs` | Library entrypoint. Public API + crate-level `//!` docs. |
| `crates/<name>/src/main.rs` | Binary entrypoint (for `--bin` crates). |
| `crates/<name>/src/<module>.rs` | Submodules. |
| `crates/<name>/tests/` | Integration tests. One file per feature; each compiles as a separate binary. |
| `crates/<name>/benches/` | Benchmarks (Criterion or built-in `#[bench]`). Optional. |
| `crates/<name>/examples/` | Runnable examples. `cargo run --example <name>`. |

## Naming conventions

| Item | Convention | Example |
|------|------------|---------|
| Crate directory | `kebab-case` | `crates/data-store` |
| Crate identifier (in code) | `snake_case` | `use data_store::…` |
| Files / modules | `snake_case.rs` | `user_repository.rs` |
| Types, traits, enums | `PascalCase` | `UserRepository`, `RepoError` |
| Functions, methods, locals | `snake_case` | `find_by_id`, `db_pool` |
| Constants, statics | `SCREAMING_SNAKE_CASE` | `MAX_RETRIES` |
| Lifetime parameters | short `'lowercase` | `'a`, `'src` |
| Type parameters | short `PascalCase` | `T`, `K`, `Ctx` |

## Module declaration pattern

Submodules are declared in the parent module (`lib.rs` or another `mod.rs`-style
file). Prefer one file per module over `<module>/mod.rs`:

```rust
// crates/data-store/src/lib.rs
pub mod repository;
pub mod error;
```

```text
crates/data-store/src/
├── lib.rs
├── repository.rs
└── error.rs
```
