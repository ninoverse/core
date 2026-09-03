# Commit Conventions

Follow the [Conventional Commits](https://www.conventionalcommits.org/) specification.

## Format

```
<type>(<scope>): <description>

[optional body]
```

- Subject line: max 72 characters, lowercase, no trailing period
- Use imperative mood: "add feature" not "added feature"
- Body: wrap at 72 characters, explain *why* not *what*

## Types

| Type | When to use |
|------|-------------|
| `feat` | New feature or user-visible behaviour |
| `fix` | Bug fix |
| `refactor` | Code change with no behaviour change |
| `style` | Formatting, whitespace — no logic change |
| `docs` | Documentation only |
| `chore` | Build scripts, deps, tooling, CI |
| `perf` | Performance improvement |
| `revert` | Reverts a previous commit |

Append `!` after the type for breaking changes: `feat!: bump MSRV to 1.85`.

## Scopes (optional but recommended)

Use the crate name or layer being changed: `<crate-name>`, `workspace`, `ci`, `deps`, `config`.

## Examples

```
feat(data-store): add async batch insert API
fix(http-client): retry budget leak under timeout
refactor(workspace): move shared error type into errors crate
chore(deps): bump tokio to 1.40
docs: document MSRV policy in CLAUDE.md
feat!: bump MSRV to 1.85
```

## Before every commit

Check whether the change makes anything in `TODO.md` or `docs/` false. If it
does, fix it in the same commit. See `.claude/doc-upkeep.md`.

## What to avoid

- Vague messages: `fix stuff`, `update`, `wip`
- Mixing unrelated changes in one commit
- Committing secrets or credentials (they are gitignored for a reason)
- Leaving a doc describing behaviour the commit just changed
