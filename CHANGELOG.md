# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.1.0] - 2026-05-29

### Added

- **Connection safety (v1.1 #2)**: optional `color`, `confirm_writes`
  and `read_only` fields on `[[connection]]`. The active connection's
  name is tinted in the status bar; writes to confirm-marked
  connections require typing `YES`; read-only connections reject
  non-SELECT batches at the syntactic guard and via
  `set_read_only(true)` on the driver session.
- **`:goto` fuzzy navigator (v1.1 #1)**: Ctrl-N / `:goto` / `:g` opens
  a Helix-style fuzzy matcher over every schema / table / view across
  all open sessions. Now correctly handles non-ASCII identifiers
  (Turkish, Cyrillic, CJK).
- **Explain tree visualiser (v1.1 #3)**: cost bars and hot-path
  colouring for `EXPLAIN` output.
- **`:submit` / `:revert` (v1.2 #5)**: command aliases to flush or
  discard the pending-mutation queue.
- **Foreign-key navigation (v1.2 #6)**: `f` (or `gd`) in the results
  pane on a foreign-key cell opens a new SELECT scoped to the
  referenced row. Identifiers are dialect-quoted and the cell value
  is bound as a query parameter — no string interpolation.
- **Result palette filters (v1.2 #7)**: `:filter <expr|clear>` and
  `:sort <N|clear>` expose the in-memory filter/sort layer through
  the command palette.
- **Schema diff migration generator (v1.2 #8)**: `:diff <a> <b>`
  compares two connections and emits ALTER TABLE statements.
- **SQL linter (v1.3 #9)**: `:lint` flags SELECT *, UPDATE/DELETE
  without WHERE, TRUNCATE, and FROM-comma Cartesian joins. The
  destructive-no-where rule now goes through the statement splitter,
  so a `;` inside a string literal no longer fragments the source
  into a false-positive UPDATE.
- **Templates and history search (v1.3 #10–12)**: `:tpl` inserts
  built-in templates (sel / ins / upd / del / join / with);
  `:history [pattern]` opens a pre-filtered Ctrl-R modal.
- `ConnectionParams::with(|p| { ... })` builder helper so callers
  outside `narwhal-core` can construct the struct without struct
  literal syntax (it is now `#[non_exhaustive]`).

### Changed

- `ConnectionParams` is marked `#[non_exhaustive]`. Future field
  additions stay non-breaking. Migrating: replace `ConnectionParams
  { ..Default::default() }` with `ConnectionParams::with(|p| { ... })`.
- `RunRequest` now carries a `params_per_statement` vector and
  exposes `RunRequest::new` / `RunRequest::with_params` so internal
  callers (foreign-key nav, future programmatic dispatch) can route
  bound parameters end-to-end through `spawn_run`.
- Cargo description: "Multi-driver TUI database client with a
  built-in MCP server." (Was a tongue-in-cheek DataGrip comparison
  that oversold the v1.0 surface.)

### Fixed

- **C1**: Goto fuzzy navigator no longer panics on non-ASCII table
  names. The previous `Utf32Str::Ascii(s.as_bytes())` shortcut
  interpreted UTF-8 bytes as ASCII code units.
- **C2**: Foreign-key navigation is no longer vulnerable to SQL
  injection through the cell value or through unusual identifier
  characters. Identifiers are dialect-quoted; the value is bound as
  a query parameter.
- **M1**: Ctrl-N inside an open completion popup advances the popup
  (mirroring vim / IDE convention) instead of stealing focus to the
  `:goto` modal. Ctrl-P added as the inverse.
- **M2**: The lint rule for destructive-without-WHERE no longer
  splits the source on every `;`. Statements containing literal
  semicolons in string literals are kept whole, eliminating false
  positives and missed cases.

## [1.0.0]

### Added

- `SECURITY.md` with private disclosure policy, scope, and hardening
  notes for operators.
- `CONTRIBUTING.md` covering workflow, commit conventions, code style,
  and the per-PR checklist.
- `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1).
- GitHub issue templates (bug report, feature request) and a PR
  template.
- `dependabot.yml` for weekly cargo + GitHub Actions updates with
  sensible grouping.
- `docs/img/demo.gif` recorded with VHS and reproducible from
  `docs/img/demo.tape` + `docs/img/seed-demo-db.sh`. Hero asset for
  the README.

### Changed

- README tagline now leads with the built-in MCP server: "A TUI
  database client with a built-in MCP server. Five databases, vim
  editing, Lua plugins."
- The MCP section moved up next to Quick Start so it lands above the
  fold for first-time readers.
- Replaced the static `hero.png` at the top of the README with the new
  animated demo.
- Halved the em-dash count in the README's upper section for cleaner
  scanning.
- Install section now lists Cargo / cargo-binstall / Homebrew tap /
  AUR / Nix as first-class options and drops the "post-2.0 roadmap"
  language for AUR + Homebrew now that the packaging templates land
  with v1.0.
- `packaging/homebrew/narwhal.rb`: dropped runtime `postgresql` /
  `mysql-client` dependencies (drivers link statically); kept only
  `rust` + `cmake` + `llvm` as build deps. Now uses `std_cargo_args`.
- `packaging/aur/PKGBUILD`: switched to the standard `prepare/build/
  check/package` layout, ships both LICENSE-MIT and LICENSE-APACHE,
  installs the README under `share/doc/`.

### Added (continued)

- `cargo-binstall` metadata on the binary crate so users without a
  Rust toolchain can grab the prebuilt tarball produced by
  `.github/workflows/release.yml`.

### Changed (continued)

- The binary crate is now published as `narwhaldb` on crates.io. The
  bare `narwhal` slot was squatted in 2018 by an abandoned docker
  library and the name cannot be reclaimed without a multi-month
  adoption procedure. The installed command name is unchanged (still
  `narwhal`); only the install incantation differs:
  `cargo install narwhaldb` instead of `cargo install narwhal`.
- README + release tarball naming now use `narwhal-X.Y.Z-<target>`
  (matching what release.yml has always produced); an earlier
  `narwhal-vX.Y.Z-` example in the README was a typo and pointed at a
  download path that doesn't exist.

### Fixed

- `core::dispatch`: trailing-expression in `Command::Substitute` arm
  now ends with `;` to satisfy `clippy::semicolon_if_nothing_returned`.

## [1.0.0] — 2026-05-24

First public release.

### L36: DataGrip-parity feature pack

First wave of editor-first quality-of-life features inspired by
`lazysql`, plumbed through narwhal's existing Action / Effect /
RowSource / dialect-aware quoting stack. Every entry below ships with
integration tests; the workspace keeps clippy-clean under `pedantic +
nursery + -D warnings`.

#### Added

- **Row CRUD + pending changes pipeline (#1).** `o` queues an empty
  insert, `O` duplicates the focused row, `d` queues a delete, cell
  edit (`e` + `Enter`) now queues an `UPDATE` instead of hitting the
  database. `Ctrl-S` commits every staged mutation in a single
  transaction (or savepoint when the user is already inside one);
  `Ctrl-X` discards the queue; `Ctrl-P` (or `:pending` / `:diff`)
  toggles a preview modal showing the generated SQL in order.
  Optimistic concurrency is encoded into every WHERE clause so a
  concurrent edit fails the commit instead of silently overwriting.
  Primary-key guard refuses `d` on tables without a PK with a
  user-readable message.
- **Metadata tabs in TableDetail (#2).** Sidebar `Enter` on a table
  opens a five-tab view: Records · Columns · Constraints · Foreign
  Keys · Indexes. `1`–`5` chord switches the active tab; the sidebar
  auto-focuses the Results pane so the chord lands on the right
  widget. Backed by the existing schema queries; no new driver
  surface required.
- **Built-in JSON viewer (#3).** `z` opens the focused cell in a
  full-screen modal, `Z` opens the whole row. `j/k/Ctrl-D/Ctrl-U/g/G`
  scroll, `y/Y` yank, `q/Esc` close. Pretty-prints valid JSON via
  `serde_json` and falls back to the raw payload for non-JSON cells.
- **Action + Keymap layer (#4).** New `narwhal_commands::action`
  (Action enum + KeyGroup taxonomy) and `narwhal_commands::keymap`
  (registry + chord parser). `[keymap.<group>]` overrides in
  `config.toml` rebind any chord; malformed entries surface as
  warnings instead of panicking. v1 wires the override pipeline
  end-to-end with an integration test and exposes the live keymap
  through `AppCore::keymap()` for help/config tooling.
- **History modal enrichment (#5).** Ctrl+R now shows an outcome
  glyph (● green/yellow/red), elapsed timing (auto-scaled ms/s/m),
  and rows summary (↓N for returned, ∼N for affected) so the user
  can spot slow and failed queries at a glance. Filtering, j/k
  navigation and Enter-to-paste were already wired in v0.
- **`${env:VAR}` interpolation (#6).** Connection params, SSH config
  and SSL certificate paths now accept `${env:NAME}` and
  `${env:NAME:fallback}` placeholders. Fallbacks may themselves be
  `${env:…}` references up to a depth of 8. Missing variables
  surface as `ConfigError::Interpolate` so the failure is visible
  immediately, not buried in a downstream engine error.
- **Pre-connect commands (#7).** Each connection can carry an ordered
  list of `[[connections.pre_connect]]` shell steps that run before
  the SSH tunnel and the driver. Each step's stdout is optionally
  captured into a named variable (`save_output_to`) and exposed to
  the rest of the connection params via `${preconnect:NAME}`
  placeholders. Per-step `timeout_secs` (default 30) and `required`
  (default true) flags bound execution.
- **`--read-only` flag (#11).** Refuses every row-level mutation
  regardless of the driver's `row_level_dml` capability. The TUI
  shows an `[RO]` badge in the status bar; the `exec` subcommand
  refuses `--write` while `--read-only` is in effect.
- **Pending mutations badge.** Status bar shows `⏳N pending` whenever
  the staged-mutation queue is non-empty. Uses the same style as the
  transaction badge so both "uncommitted state" cues read the same
  way.
- **Audit log for pending commits.** Each committed mutation lands in
  the journal as a separate `HistoryEntry` tagged `source = "pending"`.
  Failures attach the engine error to every statement in the batch.
- **`row_level_dml` capability flag.** Postgres / SQLite / MySQL /
  DuckDB opt in; ClickHouse declines and the row CRUD pipeline
  refuses staging with engine-specific guidance.
- **`:pending` / `:diff` command.** Discoverable counterpart to the
  `Ctrl-P` chord for users who navigate by command line.

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


