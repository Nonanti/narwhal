# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Architecture refactor

The workspace was reorganised around a strict view / domain / app /
driver split. No user-facing behaviour changes; the binary's CLI,
keymap, config schema and MCP protocol are unchanged.

#### Added

- **`narwhal-driver-registry`** crate. Single home for the
  `DriverRegistry` previously duplicated in `narwhal-app` and
  `narwhal-mcp`. Bundled drivers are now opt-in via cargo features
  (`driver-postgres`, `driver-sqlite`, `driver-mysql`, `driver-duckdb`,
  `driver-clickhouse`, `all-drivers`).
- **`narwhal-domain`** crate. Pure model state with no IO and no
  rendering. Initial residents: `EditorBuffer` and its support types,
  the `SchemaListing` type alias.
- **`narwhal-commands`** crate. Stateless command and helper modules:
  command dispatch, completion engine, export pipeline, connection
  wizard, snippet store, DDL/EXPLAIN helpers, inline cell edit,
  statement extraction, meta queries, session types.
- **Build matrix.** The binary defaults to `["driver-postgres",
  "driver-sqlite"]`; downstream packagers can pick the exact driver
  set they want. `cargo build -p narwhal --no-default-features
  --features driver-sqlite` produces a minimal SQLite-only build.
- **`docs/ARCHITECTURE.md`** — target layer diagram, state ownership
  table, dependency rules, feature matrix.
- **`docs/STYLE.md`** — single-source-of-truth code style: no
  AI-cliché comments, file / function size limits, lint allow-list,
  error / async / logging rules.

#### Changed

- **narwhal-app shrunk from 12 391 LOC to 6 909 LOC** (−44 %). The
  remaining code is the genuine event loop and `AppCore` glue.
- **`AppCore` god-struct cracked open.** `core/mod.rs` went from
  1 498 LOC to 150 LOC. State type definitions moved to
  `core/state/{result, tab, sidebar, history, snippets_modal,
  status}`. The `impl AppCore` block split into `construct.rs`
  (constructors, settings, sidebar rebuild), `accessors.rs`
  (read-only getters), `dispatch.rs` (render + key/mouse +
  `:`-prompt).
- **`editor_dispatch.rs`** (1 066 LOC) split into a directory:
  `mod.rs` (global dispatcher), `editor_keys.rs`, `search.rs`,
  `completion.rs`, `sidebar.rs`.
- **`wizard.rs`** (930 LOC) split into a directory: `mod.rs`,
  `fields.rs`, `state.rs`, `logic.rs`, `path.rs`.
- **narwhal-plugin-lua no longer depends on narwhal-core directly.**
  Plugin runtimes consume the narrow surface exported by
  `narwhal-plugin`. Future runtimes (WASM, native Rust) follow the
  same one-way edge.
- **narwhal-tui split.** `widgets/editor.rs` (1041 LOC) is now 341 LOC
  — only render code. The text buffer model lives in
  `narwhal-domain::editor`. `widgets/results.rs` (1301 LOC) is now a
  module with seven files (sort, model, cells, schema_detail, popups,
  table_paint, mod).
- **narwhal-commands modules split.** `export.rs` (1332 LOC) →
  `export/{csv,json,tsv,table,insert,quoting,source,format,error}`.
  `completion.rs` (1041 LOC) →
  `completion/{context,tokenizer,items,keywords,gather}`.
- **Workspace lints upgraded** to `clippy::pedantic` + `clippy::nursery`
  with a documented allow-list, and the build now passes under
  `cargo clippy --workspace --all-targets -- -D warnings` with zero
  warnings. Style-only lints with false-positive heavy reports
  (`match_same_arms`, `significant_drop_tightening`,
  `option_if_let_else`, `items_after_statements`, `too_many_lines`,
  ...) are documented allow-entries in the workspace `Cargo.toml`.
  Production `unwrap`/`expect` sites in `core/results_actions`,
  `core/sessions` and `core/transactions` rewritten as `let-else`.
- **Workspace formatted** with `cargo fmt --all`; the CI step
  `cargo fmt --check` now passes.
- **Rustdoc clean** under `RUSTDOCFLAGS='-D warnings'`; stale
  intra-doc links pointing at moved items rewritten.
- **File renames** that fixed a long-standing naming collision:
  `narwhal-app/src/edit.rs` → `cell_edit.rs`, `editor.rs` →
  `statements.rs`, `core/editor_handlers.rs` →
  `core/editor_dispatch.rs`.

#### Removed

- 120 banner comment lines (`// ===`, `// ---`, `// ***`) across the
  workspace per the new style guide.
- `#[non_exhaustive]` from workspace-internal enums that now cross
  crate boundaries (these are internal types, not public API).

## [1.1.0] — 2025

The 1.1 release line predates this file's rewrite. See the git history
(`git log --oneline v1.0.0..v1.1.0`) for individual commits.

## [1.0.0] — 2025

Initial release. Three-phase development: phase 1 (engine), phase 2
(navigation + table editing), phase 3 (multi-driver, export, plugins,
headless mode, MCP server).
