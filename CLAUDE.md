# CLAUDE.md

This file provides strict guidance and architectural rules for Claude Code (claude.ai/code) when working in this repository.

## Commands & Tooling

- **Toolchain:** Rust is pinned via `rust-toolchain.toml` (channel = `stable`). Every contributor automatically gets the latest stable toolchain on first `cargo` invocation. Required components: `rustfmt`, `clippy`.
- **Maintain the Build:** Never leave the codebase in a state where build, lint, or tests fail. Run the relevant commands below to verify your work before concluding a task.

```bash
cargo build --workspace                                                # Build all crates
cargo watch -x 'check --workspace'                                     # Dev loop (requires cargo-watch)
cargo build --workspace --release                                      # Release build
cargo clippy --workspace --all-targets --all-features -- -D warnings   # Lint
cargo fmt --all                                                        # Format (check-only: `cargo fmt --all -- --check`)
cargo nextest run --workspace                                          # Tests (fallback: cargo test --workspace)
cargo deny check                                                       # Licenses + advisories
cargo audit                                                            # CVE check
```

Install the auxiliary tools once per machine:

```bash
cargo install --locked cargo-nextest cargo-watch cargo-deny cargo-audit
```

## Architecture & Workspace Rules

**Layout:** Cargo workspace, edition `2021`. New code goes in a crate under `crates/<name>/`. The workspace root `Cargo.toml` declares `members = ["crates/*"]` and centralizes shared metadata under `[workspace.package]` and shared dependencies under `[workspace.dependencies]`.

**Crate inheritance:** Crate manifests inherit shared keys from the workspace using `<key>.workspace = true` (e.g. `edition.workspace = true`, `license.workspace = true`). Shared dependencies are referenced as `<crate> = { workspace = true }`.

**MSRV:** Pinned in `clippy.toml` and `[workspace.package].rust-version`. Do not bump it incidentally.

## Behavioral Guidelines

**Tradeoff:** Bias toward caution over speed. For trivial tasks, use judgment.

### 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

- State your assumptions explicitly. If uncertain, stop and ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, propose it. Push back when warranted.

### 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked. No abstractions for single-use code.
- No "flexibility" or error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

### 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

- Don't "improve" adjacent code, comments, or formatting.
- Match existing style exactly.
- Remove imports/variables/functions that YOUR changes made unused. Don't remove pre-existing dead code unless asked.

### 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

- Transform tasks into verifiable goals (e.g., "Add validation" → "Write tests for invalid inputs, then make them pass").
- For multi-step tasks, state a brief plan and verify each step independently.

---

## Extended Rules (Read Before Acting)

Use your file-reading capabilities to read the exact rules in the `.claude/` directory **before** executing any of the following tasks:

- **Committing code:** Read `.claude/commit-conventions.md`
- **Creating branches:** Read `.claude/branch-naming.md`
- **Reviewing PRs:** Read `.claude/code-review.md`
- **Testing/Verifying:** Read `.claude/testing-requirements.md`
- **Opening PRs:** Read `.claude/pr-guidelines.md`
- **Creating new files:** Read `.claude/file-naming.md`
- **Building a crate or module:** Read `.claude/crate-workflow.md`
- **Deciding what to build next / branching strategy:** Read `.claude/execution-order.md`
