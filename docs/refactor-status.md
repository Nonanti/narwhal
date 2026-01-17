# Refactor Status

Live progress tracker. Updated after every commit.

| Phase | State | Tag |
|-------|-------|-----|
| 0 — Standards baseline | done | `refactor-phase-0-done` |
| 1 — Feature flags + driver registry | not started | — |
| 2 — Rename collisions | not started | — |
| 3 — Extract narwhal-domain | not started | — |
| 4 — Extract narwhal-commands | not started | — |
| 5 — Plugin isolation | not started | — |
| 6 — Binary slimming + final pass | not started | — |
| 7 — Docs + CHANGELOG rewrite | not started | — |

## Open notes

- Start tag: `narwhal-refactor-c-start`.
- CHANGELOG will be rewritten from scratch in Phase 7.
- No user-facing changes throughout. Tests must stay green at every phase exit.

### Phase 0 outcome

- Workspace lints upgraded: `clippy::pedantic` + `clippy::nursery` enabled
  with a documented allow-list. Production-only lints (`unwrap_used`,
  `expect_used`, `panic`, `dbg_macro`, `print_stdout`, `print_stderr`,
  `todo`, `unimplemented`) live on each `lib.rs`/`main.rs` so test code
  stays ergonomic.
- `cargo clippy --workspace --fix` applied for lib + bin targets.
  Warnings went from 635 → 311. Remaining warnings are mostly
  missing-Debug, too-many-lines, identical-match-arms — these will be
  resolved naturally as the god crate is split in Phases 3-4.
- 120 banner comments (`// ===`, `// ---`, `// ***`) stripped per
  `docs/STYLE.md`.
- Test suite green: full `cargo test --workspace --lib` passes.
