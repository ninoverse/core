# Execution Order & Branching Strategy

Defines the branch / PR structure for work in this workspace.

---

## Branching and PR strategy

See `.claude/branch-naming.md` for the branch name format.

| Work type | Branch prefix | One PR per |
|-----------|--------------|-----------|
| Foundation scaffold | `chore/` | whole scaffold |
| Toolchain / config bump | `chore/` | one PR |
| Crate group | `feat/` | group (e.g. `feat/storage-crates`) |
| Single isolated crate | `feat/` | crate |
| Rename / refactor | `refactor/` | logical rename unit |
| Docs / rules | `docs/` | one PR |

**Draft PR rule:** open a draft PR at the group's **first commit**. Push every
subsequent commit to that same PR. Mark ready for review only when these all
pass cleanly:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace
```

…and `TODO.md` plus every affected `docs/` page have been reconciled against the
code the group actually landed (`.claude/doc-upkeep.md`).

---

## Within each group

- Build **one crate at a time**.
- Follow the 9-step checklist in `.claude/crate-workflow.md` for each.
- Stop and confirm with the user after each crate before starting the next.
- Existing crates in scope get an **audit-pass** (clippy + tests + a read-through);
  only commit if a real defect is found.

## Audit-pass checklist (existing crates)

1. Open the crate's `Cargo.toml` and `src/lib.rs` — check for outdated deps,
   missing doc comments, `unwrap()` in non-test paths.
2. Run `cargo clippy -p <crate> --all-targets -- -D warnings` and
   `cargo nextest run -p <crate>`.
3. Surface anything broken. Only commit if a fix is needed; use an isolated commit.
