# Testing Requirements

## Before merging any change

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo nextest run --workspace`  *(falls back to `cargo test --workspace` if nextest is not installed)*
- [ ] `cargo deny check` *(licenses + advisories)*

All four must pass before marking a PR ready for review.

## Test layout

| Test type | Location | When to use |
|-----------|----------|-------------|
| Unit | `#[cfg(test)] mod tests { … }` inline at the bottom of the file under test | Testing private functions or internal logic |
| Integration | `crates/<name>/tests/<feature>.rs` | Testing the crate's public API end-to-end. Each file is compiled as a separate binary. |
| Doc test | `///` doc comment on a public item | Verifying that documented usage examples actually compile and run |
| Property | with `proptest` crate, inside unit or integration tests | Invariant-style tests across a generated input space |
| Benchmark | `crates/<name>/benches/<name>.rs` | Performance regression tracking. Optional. |

## Running specific test types

```bash
cargo test --doc                              # doc-tests only
cargo nextest run -p <crate>                  # one crate
cargo nextest run -p <crate> <test_name>      # one test
cargo test --test <integration_file>          # one integration file
```

## Watching tests during development

```bash
cargo watch -x 'nextest run --workspace'
```
