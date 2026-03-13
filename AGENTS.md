# Agent instructions for asmjson

## Code style

Always run `cargo fmt` before committing any Rust source changes:

```
cargo fmt
```

## Conversation log

Keep `doc/conversation.md` up to date as work progresses.  After each
meaningful unit of work (a feature, a fix, a refactor, a profiling session)
append a new section to the file describing:

- **What was done** — the change or investigation and why it was undertaken.
- **Design decisions** — alternatives considered and the reasoning behind the
  chosen approach.
- **Results** — benchmark numbers, test outcomes, perf data, or other
  measurements where relevant.
- **Commit** — the abbreviated hash and subject of the resulting commit.

Use second-level headings (`##`) for sessions and third-level headings (`###`)
for individual topics within a session.  Do not rewrite earlier sections;
append only.
