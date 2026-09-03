# Crate Workflow

The exact procedure for adding or modifying a single crate in this Cargo
workspace. Follow every step in order; do not skip or reorder.

---

## Pre-flight

Before writing any code:

1. **Ask for confirmation.** State which crate you are about to add and what it
   will contain. Wait for explicit approval. Do not start on your own initiative.

2. **Check if the crate already exists:**
   ```bash
   ls crates/<name>/Cargo.toml 2>/dev/null && echo EXISTS || echo MISSING
   ```
   If it exists, report the finding and ask: skip / overwrite / modify.
   Never silently overwrite.

3. **Read `TODO.md`** and any `docs/` page covering the area you are about to
   touch, so you work from current state rather than a stale claim. See
   `.claude/doc-upkeep.md`.

---

## 9-step checklist (one crate, one commit)

Complete all nine steps before committing. Never commit a partial crate.

### 1. Scaffold the crate

```bash
cargo new --lib crates/<name>     # use --bin for an executable instead
```

Crate directory naming: `kebab-case` (e.g. `crates/data-store`). The crate's
identifier in code becomes `snake_case` (`use data_store::…`).

### 2. `crates/<name>/Cargo.toml`

Inherit shared metadata from the workspace:

```toml
[package]
name = "<name>"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
# Shared deps come from the workspace:
# serde = { workspace = true }
```

### 3. `crates/<name>/src/lib.rs` (or `main.rs`)

- Public API surface only. Implementation lives in submodules.
- Every `pub` item gets a `///` doc comment.
- Crate-level docs go in a `//!` block at the top of the file.

### 4. Module split

Any item > ~150 LOC moves to its own file: `crates/<name>/src/<module>.rs`,
declared with `mod <module>;`. Filenames are `snake_case.rs`.

### 5. Unit tests

Inline at the bottom of the file under test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_expected_result() {
        assert_eq!(do_thing(2), 4);
    }
}
```

### 6. Integration tests + doc tests

- Integration tests: `crates/<name>/tests/<feature>.rs`. Each file compiles as
  a separate binary against the crate's public API.
- Doc tests: every public function gets a runnable example in its `///` doc
  comment unless the behavior is trivially obvious from the signature.

### 7. Workspace wiring

The workspace `Cargo.toml` already globs `crates/*`, so new crates are picked
up automatically. If this crate depends on another workspace crate, add it
with a path dependency:

```toml
[dependencies]
other-crate = { path = "../other-crate" }
```

### 8. Verification gate

All three must pass before committing:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace        # falls back to `cargo test --workspace`
```

Then reconcile the docs: if this crate makes anything in `TODO.md` or `docs/`
false, fix it in this same commit (`.claude/doc-upkeep.md`).

### 9. Commit + push + draft PR

```
feat(<crate>): add <name> crate
```

One crate per commit. Never batch multiple crates in one commit.

- Push the commit to the current group branch.
- If this is the **group's first commit**: open a draft PR immediately.
- If the draft PR already exists: just push to it.
- **Stop.** Ask before starting the next crate.

---

## Group verification gate

Run before marking any group PR ready for review:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace
cargo deny check
```

All four must pass cleanly with zero warnings.

Then re-read `TODO.md` and every `docs/` page naming a module this group
changed, and reconcile before marking the PR ready (`.claude/doc-upkeep.md`).
