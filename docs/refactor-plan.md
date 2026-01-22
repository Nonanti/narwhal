# Narwhal Refactor Plan

Scope: full architectural cleanup ("Plan C"). No new features. No user-facing changes.

Pre-reqs:
- Tag `narwhal-refactor-c-start` set (rollback anchor).
- All work happens on the current branch. Each phase ends with a green `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`.
- Each numbered task becomes one commit (conventional commits).
- After each phase a `context_tag` is set: `refactor-phase-<n>-done`.

Legend: 🟢 mechanical · 🟡 careful · 🔴 invasive

---

## Phase 0 — Standards baseline

Goal: apply the new lints and style hygiene to every existing crate before moving code, so we never re-introduce violations during the move.

0.1 🟢 Add `docs/STYLE.md`, `docs/ARCHITECTURE.md`, `docs/refactor-plan.md` (this file). *(done in this commit)*
0.2 🟢 Add `#![forbid(unsafe_code)]` and the lint block from STYLE.md to every crate root that doesn't have it.
0.3 🟡 Run `cargo clippy --workspace -- -D warnings` with the new lints. Fix mechanical issues only (`pedantic`/`nursery` violations) in-place. Bulk `#[allow]` is forbidden; allow per-item with a one-line `// reason:` only when unavoidable.
0.4 🟢 Strip AI-cliché comments workspace-wide (rg sweep: `// elegant`, `// for now`, `// TODO: improve later`, `// helper`, `// comprehensive`, banner comments). Delete or rewrite into proper docs.
0.5 🟢 Forbid `unwrap`/`expect` in non-test code. Replace with `?` or `ok_or` against a real error variant. CI gate: `rg -n '\.unwrap\(\)|\.expect\(' --type rust -g '!tests/**' -g '!**/tests.rs'` returns nothing outside `#[cfg(test)]`.
0.6 🟢 Empty `mod.rs` files: move all logic out of `crates/narwhal-app/src/core/mod.rs` once Phase 3 starts; for now mark with a `// PHASE-3` anchor (no other comments added).

Exit: clippy clean, no `unwrap` outside tests, CHANGELOG cleared (we'll rewrite from scratch at the end). Tag: `refactor-phase-0-done`.

---

## Phase 1 — Feature flags + driver registry

Goal: drivers become opt-in and pluggable. `narwhal-mcp` stops depending on concrete driver crates.

1.1 🟡 Create `crates/narwhal-driver-registry`. Move the `Driver` trait (currently implicit, derive from concrete impls) and `DriverKind` into this crate. Define `Registry`, `Factory`, and a `register_default_drivers(&mut Registry)` entrypoint behind feature flags.
1.2 🟡 Refactor each `narwhal-driver-*` crate to implement `narwhal_driver_registry::Driver` and expose a `pub fn register(reg: &mut Registry)` function. No behaviour change.
1.3 🟡 `narwhal-pool`: depend on `narwhal-driver-registry` instead of concrete drivers.
1.4 🔴 `narwhal-app/Cargo.toml`: add `[features]` block. Drivers become optional dependencies gated by features. Code branches on registry, not on `cfg(feature = "...")` scattered through call sites.
1.5 🔴 `narwhal-mcp`: replace direct driver imports with registry lookup. Same feature flag set as the binary.
1.6 🟡 `narwhal/Cargo.toml`: re-export feature flags. Default = `["postgres", "sqlite"]`. Add `all-drivers`.
1.7 🟢 README + `docs/ARCHITECTURE.md` updated with feature matrix and minimal-build instructions.
1.8 🟢 CI: matrix entries for `default`, `--no-default-features --features sqlite`, `--all-features`.

Exit: every driver can be turned off without compile errors, MCP & App share one registry. Tag: `refactor-phase-1-done`.

---

## Phase 2 — Rename / consolidate collisions

Goal: kill the `editor.rs`/`edit.rs`/`editor_handlers.rs`/`widgets/editor.rs` confusion before we start moving code, so move diffs read cleanly.

2.1 🟡 `narwhal-app/src/edit.rs` → `narwhal-app/src/cell_edit.rs` (it's cell editing, not text editing). Update imports.
2.2 🟡 `narwhal-app/src/editor.rs` (80 LOC, editor command wiring) folds into the command module. Delete the file.
2.3 🟡 `narwhal-app/src/core/editor_handlers.rs` renamed to `narwhal-app/src/core/editor_dispatch.rs` for the duration of Phase 2; this file will be **deleted** in Phase 4 when its contents move to `narwhal-domain` and `narwhal-commands`. The rename signals "in flight".
2.4 🟢 `narwhal-tui/src/widgets/editor.rs` stays as the only "editor" name under `narwhal-tui`.
2.5 🟢 `rg "editor_handlers"` returns no hits.

Exit: `rg "editor" -l` shows ≤ 4 files, each with a clear, distinct role. Tag: `refactor-phase-2-done`.

---

## Phase 3 — Bootstrap `narwhal-domain`, move `EditorBuffer`

Goal: the domain crate exists and owns at least one real model, so later phases have somewhere to move state into. Larger AppCore / ResultView splits intentionally deferred so this phase stays surgical.

3.1 🔴 Create `crates/narwhal-domain` with zero deps beyond `narwhal-core` and `narwhal-vim`.
3.2 🔴 Move `EditorBuffer` (and its support types `EditorSearchHighlight`, `CompletionItemView`, `CompletionPopupView`, `LineCursor`, `floor_char_boundary`) from `narwhal-tui::widgets::editor` to `narwhal-domain::editor`. Domain version owns mutation; TUI side keeps render functions only.
3.3 🔴 TUI re-exports the moved types so external callers keep their imports.
3.4 🟢 Domain editor module carries the pure-model tests (insert/navigate, delete/join, word motion, multibyte boundary). TUI keeps the ratatui-coupled placement tests.

Deferred to later phases (because they touch the god crate and are safer to do alongside the commands extraction):
- ResultView ↔ ResultModel split (`TableState` stays in TUI). → Phase 6.
- AppCore field reduction. → Phase 4 alongside the commands extraction.
- Tab / Session / SidebarItem / HistoryState / CompletionState etc. relocation. → Phase 4.
- editor_dispatch.rs split. → Phase 4.

Exit: `narwhal-domain` is a real workspace crate, `narwhal-tui::widgets::editor.rs` shrunk from 1041 LOC to <400 LOC, tests green. Tag: `refactor-phase-3-done`.

---

## Phase 4 — Extract `narwhal-commands`

Goal: take the second half of the god crate out.

4.1 🔴 Create `crates/narwhal-commands`. Deps: `narwhal-domain`, `narwhal-driver-registry`, `narwhal-sql`, `narwhal-pool`, `narwhal-history`.
4.2 🔴 Move:
    - `narwhal-app/src/commands.rs` (756 LOC) → `narwhal-commands/src/dispatch.rs` (split if > 500).
    - `narwhal-app/src/completion.rs` (1045 LOC) → `narwhal-commands/src/completion/` (split into `engine.rs`, `sources.rs`, `ranker.rs`).
    - `narwhal-app/src/export.rs` (1335 LOC) → `narwhal-commands/src/export/` (split per format: `csv.rs`, `json.rs`, `sql.rs`, `pipeline.rs`).
    - `narwhal-app/src/wizard.rs` (935 LOC) → `narwhal-commands/src/wizard/` (split per step).
    - `narwhal-app/src/{snippets,explain,ddl,cell_edit,meta,session}.rs` → respective modules under `narwhal-commands`.
    - `narwhal-app/src/core/{dump_export, plugin_executor, plugins, results_actions, sessions, tabs, transactions}.rs` → `narwhal-commands` (each one becomes its own module file).
4.3 🟡 Replace direct mutation calls with `Intent → Effect` model. `narwhal-commands` returns `Effect`s; `narwhal-app::dispatch` applies them.
4.4 🟢 `narwhal-app` now contains only: `app.rs`, `run.rs`, `terminal.rs`, `registry.rs` (driver wiring), `draw_scheduler.rs`, `dispatch.rs`, `effects.rs`. Target ≤ 2000 LOC.
4.5 🟢 Workspace `Cargo.toml` lists `narwhal-commands`. README architecture section updated.

Exit: `narwhal-app` ≤ 2000 LOC, ≤ 10 files. `narwhal-commands` builds against `narwhal-domain` with no `narwhal-app` or `narwhal-tui` dependency. Tag: `refactor-phase-4-done`.

---

## Phase 5 — Plugin isolation

Goal: plugins consume a stable narrow surface, not the internals.

5.1 🟡 Define `narwhal-plugin::api` — the only types plugins see (`Snapshot`, `CommandRequest`, `CommandResponse`). Stable, semver-tracked.
5.2 🔴 `narwhal-plugin-lua`: depend only on `narwhal-plugin`. Remove `narwhal-core` direct dep. If a type is needed, expose it via `narwhal-plugin::api`.
5.3 🟢 Plugin host (`narwhal-app::plugin_host`) is the only crate that bridges `narwhal-plugin::api` and `narwhal-domain`.

Exit: `cargo tree -p narwhal-plugin-lua` shows `narwhal-plugin` only on the narwhal side. Tag: `refactor-phase-5-done`.

---

## Phase 6 — Binary slimming + final pass

6.1 🟢 `narwhal/src/main.rs` reviewed: only CLI + bootstrap. Anything else moves to `narwhal-app::run`. Target ≤ 400 LOC.
6.2 🟢 `narwhal-tui::widgets::results.rs` (1302 LOC) split per concern (`table.rs`, `header.rs`, `popup.rs`, `cell.rs`).
6.3 🟢 `narwhal-tui::widgets::editor.rs` (1042 LOC) split (`gutter.rs`, `text.rs`, `cursor.rs`, `selection.rs`).
6.4 🟢 Final lint pass with `clippy::pedantic` enabled crate-wide.
6.5 🟢 Re-derive `Cargo.lock`. Verify no unused deps (`cargo udeps` or manual grep).
6.6 🟢 Workspace-wide check: every file ≤ 500 LOC except where explicitly justified in a `docs/EXCEPTIONS.md`.

Exit: clippy pedantic clean, files within limits. Tag: `refactor-phase-6-done`.

---

## Phase 7 — Docs + CHANGELOG rewrite

7.1 🟢 README: update architecture diagram to match `ARCHITECTURE.md`.
7.2 🟢 Per-crate `README.md` (≤ 30 lines each) describing purpose and public surface.
7.3 🟢 CHANGELOG.md: rewritten from scratch as Berkant requested. New format, starting from this release.
7.4 🟢 `docs/EXCEPTIONS.md` lists any agreed-upon style deviations with rationale.

Exit: Tag: `refactor-phase-7-done` and `v1.2.0-rc1` candidate.

---

## Rollback

Any phase can be rolled back to its `refactor-phase-<n-1>-done` tag via `context_checkout`. The starting state is `narwhal-refactor-c-start`.

## Tracking

Work-in-progress checklist lives in `docs/refactor-status.md`, updated after every commit. This plan is not edited once Phase 0 ends — deltas go in `refactor-status.md`.
