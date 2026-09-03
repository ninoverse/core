# Documentation Upkeep

`TODO.md` and `docs/` describe the code. They go stale silently: nothing builds
them, nothing tests them, and a wrong line reference or a "not implemented yet"
note for shipped work is worse than no note at all — it sends the next reader
looking for a problem that no longer exists.

Treat them as part of the change, not as follow-up.

---

## When to check

| Moment | Do this |
|--------|---------|
| **Start of a session** | Read `TODO.md`. Skim any `docs/` page covering the area you are about to touch. Work from what the code says, not what the doc claims. |
| **Before a commit** | If the change makes a documented statement false, fix the doc **in the same commit**. |
| **End of a milestone** (crate done, group PR ready) | Re-read `TODO.md` and every `docs/` page naming a module you changed. Reconcile before marking the PR ready. |

A docs-only correction that follows shipped work is a `docs:` commit. A doc fix
that belongs to the change you are making is part of that commit — do not split
it out.

---

## What to reconcile

1. **Shipped work still listed as open.** Mark it `[x]` with the commit hash that
   landed it, and say what the behaviour is now. Keep the entry rather than
   deleting it; the record of *why* it was a problem is the valuable part.
2. **Claims the code contradicts.** A gap described as unfixable, a failure mode
   that no longer reproduces, a key listed as unparsed that is now parsed. Verify
   before repeating — do not carry a claim forward on faith because it was there
   before.
3. **References that no longer resolve.** See below.
4. **Paths that only exist on another branch.** Say which branch. A relative link
   that 404s on `main` is a bug in the doc.

---

## Reference code by name, never by line number

Line numbers drift on the next edit to the file and are wrong by the following
commit. Name the symbol instead — it moves with the code and it is greppable:

```markdown
<!-- no -->
`resolve_command` (`docker.rs:227-242`) rejects a malformed command.
the fallback at `docker.rs:748-753` degrades to "running".

<!-- yes -->
`resolve_command` rejects a malformed command.
the no-healthcheck fallback in `wait_for_healthy` degrades to "running".
```

For a spot inside a function that has no name of its own, describe it by
position: *"the readiness loop in `boot_service`"*, *"the `ports` loop in
`boot_service`"*.

State the file once per section (*"symbols named below are in
`src/docker.rs`"*) rather than repeating it on every reference.

---

## Verification gate

Before committing a doc change, confirm every symbol it names still exists:

```bash
grep -n '<symbol>' crates/<crate>/src/<file>.rs
```

And confirm no line references crept back in:

```bash
grep -nE '\.rs[:#]L?[0-9]' TODO.md docs/*.md    # expect no output
```
