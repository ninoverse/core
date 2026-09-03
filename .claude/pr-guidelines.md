# PR Guidelines

## Title

Follow the same format as commit messages: `<type>(<scope>): <description>`.  
Keep it under 72 characters.

## Description template

```markdown
## What
<!-- One-paragraph summary of the change -->

## Why
<!-- Motivation: bug, feature request, refactor reason -->

## How
<!-- Non-obvious implementation decisions -->

## Testing
<!-- How was this manually verified? Screenshots for UI changes. -->
```

## Rules

- One logical change per PR; split unrelated work into separate PRs
- Rebase onto `main` before requesting review (no unnecessary merge commits)
- All lint checks must pass: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo build --workspace` must succeed before marking the PR ready for review
- `cargo nextest run --workspace` must pass (or `cargo test --workspace`)
- Link to the relevant section in CLAUDE.md or a `.claude/` rule file if the PR establishes a new pattern
- `TODO.md` and any `docs/` page the PR makes stale are updated in the same PR (`.claude/doc-upkeep.md`)

## Size guidance

| Lines changed | Action |
|--------------|--------|
| < 200 | Normal review |
| 200 – 600 | Add context in the description about where to start reading |
| > 600 | Consider splitting — or at minimum call it out and justify it |

## Draft PRs

Use draft status for work-in-progress or when feedback is needed before the implementation is complete. Convert to ready only when `cargo clippy --workspace --all-targets -- -D warnings` and `cargo build --workspace` both pass.
