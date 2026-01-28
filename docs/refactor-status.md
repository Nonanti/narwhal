# Refactor Status

Live progress tracker. Updated after every commit.

| Phase | State | Tag |
|-------|-------|-----|
| 0 — Standards baseline | done | `refactor-phase-0-done` |
| 1 — Feature flags + driver registry | done | `refactor-phase-1-done` |
| 2 — Rename collisions | done | `refactor-phase-2-done` |
| 3 — Bootstrap narwhal-domain, move EditorBuffer | done | `refactor-phase-3-done` |
| 4 — Extract narwhal-commands | done | `refactor-phase-4-done` |
| 5 — Plugin isolation | done | `refactor-phase-5-done` |
| 6 — Binary slimming + final pass | done | `refactor-phase-6-done` |
| 7 — Docs + CHANGELOG rewrite | done | `refactor-phase-7-done` |

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

### Phase 1 outcome

- New crate `narwhal-driver-registry` owns the `DriverRegistry` and the
  conditional `with_defaults()` registration of bundled drivers.
- App and MCP no longer pull in driver crates directly; both consume
  the registry and forward feature flags to it.
- `narwhal` binary exposes `driver-postgres`, `driver-sqlite`,
  `driver-mysql`, `driver-duckdb`, `driver-clickhouse`, `all-drivers`
  with `default = ["driver-postgres", "driver-sqlite"]`.
- Build matrix verified: default features, `--no-default-features
  --features driver-sqlite`, and `--features all-drivers` all compile.

### Phase 2 outcome

- `narwhal-app/src/edit.rs`  -> `cell_edit.rs` (inline cell editing).
- `narwhal-app/src/editor.rs` -> `statements.rs` (SQL statement
  extraction over the editor buffer — the original name lied about
  the responsibility).
- `narwhal-app/src/core/editor_handlers.rs` ->
  `core/editor_dispatch.rs` (will be split between domain and
  commands in Phases 3-4).
- The TUI `widgets/editor.rs` keeps the editor name (genuine editor
  widget). `rg editor_handlers` returns nothing.

### Phase 3 outcome

- New crate `narwhal-domain` (deps: narwhal-core, narwhal-vim).
- `EditorBuffer` and all of its model-only support types relocated
  from `narwhal-tui::widgets::editor` to
  `narwhal-domain::editor`. TUI re-exports keep external imports
  working.
- `narwhal-tui::widgets::editor.rs` shrank from 1041 LOC to 341 LOC;
  only render code (`render_editor`, `render_completion_popup`,
  `editor_cursor_anchor`, `CompletionHitRegions`) remains.
- Domain tests cover insert/navigate, delete/join, word motion and
  the multibyte boundary helper. TUI keeps the ratatui placement
  tests.
- Larger model relocations (Tab, Session, sidebar/history/completion
  state) and the `editor_dispatch.rs` split were deferred to
  Phase 4 where they happen as a byproduct of the
  `narwhal-commands` extraction. ResultView -> ResultModel split
  deferred to Phase 6 (it requires breaking TableState ownership).

### Phase 4 outcome

- New crate `narwhal-commands`. 11 self-contained modules relocated
  out of narwhal-app: `cell_edit`, `commands`, `completion`, `ddl`,
  `explain`, `export`, `meta`, `session`, `snippets`, `statements`,
  `wizard` (5577 LOC).
- `SchemaListing` type alias moved from `narwhal-tui` to
  `narwhal-domain`. Both crates re-export it for compatibility.
- `#[non_exhaustive]` stripped from workspace-internal command enums
  now that they cross crate boundaries.
- narwhal-app shrank from 12391 LOC to 6765 LOC (about 46%
  reduction). The remaining LOC is the runtime / AppCore / event loop
  layer that genuinely belongs to the app.
- `narwhal-app::lib.rs` re-exports the moved modules so existing
  imports keep working; no caller code changed in this commit.

### Phase 5 outcome

- `narwhal-plugin-lua` no longer depends directly on `narwhal-core`.
  `cargo tree -p narwhal-plugin-lua` lists `narwhal-plugin` as the
  only narwhal-side crate. Plugin runtimes see the contract, not the
  internals.
- `narwhal-plugin` continues to re-export the narrow `narwhal-core`
  surface (`ColumnHeader`, `Row`, `Value`, `QueryResult`) plugins
  need, so the API remains stable while the dependency edge stays
  one-way.

### Phase 6 outcome

- `narwhal/src/main.rs` is 358 LOC, under the 400 LOC target.
- `narwhal-commands/src/export.rs` (1332 LOC) split into nine files
  under `export/`: csv, json, tsv, table, insert, quoting, source,
  format, error.
- `narwhal-commands/src/completion.rs` (1041 LOC) split into six
  files under `completion/`: context, tokenizer, items, keywords,
  gather, mod.
- `narwhal-tui/src/widgets/results.rs` (1301 LOC) split into seven
  files under `results/`: sort, model, cells, schema_detail,
  popups, table_paint, mod.
- Second `cargo clippy --fix` pass against the new layout.
- 692 tests across the workspace remain green.

Deferred items resolved in a follow-up pass:
- **AppCore strip** — `core/mod.rs` 1498 → 150 LOC. Type
  definitions moved to `core/state/*`. `impl AppCore` block split
  into `construct.rs`, `accessors.rs`, `dispatch.rs`.
- **`editor_dispatch.rs`** (1066 LOC) split into a directory:
  `mod.rs` (global dispatcher), `editor_keys.rs`, `search.rs`,
  `completion.rs`, `sidebar.rs`.
- **`wizard.rs`** (930 LOC) split into a directory: `mod.rs`,
  `fields.rs`, `state.rs`, `logic.rs`, `path.rs`.

Still outstanding (lint hygiene, not architecture):
- ~300 `pedantic` / `nursery` warnings, mostly mechanical and
  collected per-file as code is touched in follow-up work.

### Phase 7 outcome

- README architecture section rewritten to match the new layered
  map (domain / commands / app / driver-registry).
- Per-crate `README.md` added to every workspace member (19 files,
  ~30 lines each).
- `CHANGELOG.md` rewritten from scratch as Berkant requested. New
  `[Unreleased]` entry documents the refactor in full; the legacy
  entries collapse into 1.1 / 1.0 release lines pointing at git
  history.
- `docs/EXCEPTIONS.md` lists every file that breaks the 500-LOC
  soft limit with a rationale, plus the workspace clippy
  allow-list.
