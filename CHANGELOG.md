# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Operator notes

- **Audit log rotation suffix format** (MR-N4): rotated files
  previously carried a seconds-resolution timestamp
  (`audit.log.20260604T143012Z`); they now carry a millisecond
  timestamp (`audit.log.20260604T143012.473Z`), with a numeric
  `-N` suffix on the unlikely sub-millisecond collision. Any log
  shipper that greps for the old pattern needs its regex updated
  to accept `.\d{3}Z` (or `.\d{9}Z` for the nanosecond fallback).
- **MR-N3 ToolDescriptor**: `name` and `description` are now
  `Cow<'static, str>` for zero-alloc round-trips on built-in
  tools. JSON wire shape is unchanged.
- **MR-N9 LspError::Timeout**: now carries the method name, so
  status-bar surfacing reads `"LSP request 'textDocument/hover'
  timed out"` instead of a generic message.

## [2.0.0] - 2026-06-04

Major v2.0 release: full Tier 0 + Tier 1 + Tier 2 of the v2.0 roadmap.
Breaking changes are concentrated in Tier 0 (edition 2024 / MSRV 1.85,
Connection trait native `async fn`, driver crate consolidation, settings
schema v2, API audit). Tier 1 ships feature work without semantic
breaks; Tier 2 is pure polish + ecosystem expansion.

### Highlights

- **Streaming results** (`QueryStream`) — rows arrive incrementally;
  the result pane ticks live. T1-T4-A.
- **MSSQL driver** via `tiberius`. T1-T2-A.
- **Connection vault v1** (HashiCorp + 1Password). T1-T2-B.
- **Treesitter SQL parser** — scope-aware completion + cursor
  context. T1-T3-A.
- **Workspace persistence** — tabs / cursor / sidebar restore across
  launches. T1-T3-B.
- **Parquet + Markdown exporters**. T1-T4-B.
- **WASM plugin runtime** (`wasmtime` + capability sandbox v2).
  T1-T5-A / T1-T5-B.
- **Audit log** — append-only JSONL sink for SOC2 / ISO 27001
  evidence. T2-T2-D.
- **Schema diff** — `:schema-diff src tgt` and headless
  `narwhal schema-diff` emit dialect-specific migration DDL.
  T2-T2-C.
- **Inline ASCII charts** — `:chart bar|line|sparkline` over any
  result. T2-T4-C.
- **Pivot table** — `:pivot rows=.. cols=.. value=.. agg=..` with
  count / sum / avg / min / max aggregators. T2-T4-D.
- **Multi-cursor editing** — Alt-N / Alt-A / Esc collapses; insert
  and delete propagate across cursors. T2-T3-D (MVP; vim block-
  visual + undo-as-one-step deferred to v2.1).
- **Embedded LSP client crate** (sqls / sqlls protocol primitives).
  Editor wiring deferred to v2.1. T2-T3-C.
- **Plugin-defined MCP tools** — host-side registration with
  collision policy; WIT bridge deferred to v2.1. T2-T5-C.

### Breaking

- **MSRV bumped to 1.85**; edition 2024 throughout. T0-01.
- **`Connection` trait uses native `async fn` (RPITIT)**; downstream
  drivers no longer depend on `async-trait`. T0-02.
- **Driver crate consolidation**: the six per-backend crates
  collapsed into `narwhal-drivers` with one cargo feature per
  backend. T0-03.
- **Settings schema v2** with the `narwhal migrate-config` helper
  to bring v1 files forward. T0-04.
- **API surface audit** — several previously-public types are now
  `pub(crate)`. T0-05.

### Added

- New workspace members: `narwhal-audit`, `narwhal-schema-diff`,
  `narwhal-pivot`, `narwhal-lsp`, `narwhal-plugin-wasm`.
- `narwhal audit tail` CLI for the audit JSONL sink.
- `narwhal schema-diff` CLI (headless complement to the TUI command).
- `narwhal migrate-config` CLI.

### Deferred to v2.1

- Multi-cursor: vim block-visual interop, undo/redo as a single step,
  Alt-Click and column-mode chords.
- LSP client wiring into the editor pane (completion popup, hover
  tooltip, definition jump, server restart logic).
- MCP plugin-tool WIT bridge (WIT world v0.2.0), JSON-schema input
  validation, `mcp.register` capability, example WASM plugin.

## [1.2.0] - 2026-06-02

### Fixed

- **Diagram renderers** now escape control characters in edge
  labels: a column or table identifier containing a literal `\n`
  (legal in PostgreSQL via quoted identifiers) used to break Mermaid
  parsing with an "unexpected token" and silently mangle DOT edges.
  Mermaid downgrades newlines / tabs to spaces; DOT escapes them as
  the literal `\n` / `\r` / `\t` glyphs the Graphviz parser expects.
- **Mermaid title sanitiser** strips the `---` token so a title
  containing the YAML front-matter delimiter cannot close the block
  early and inject a bogus `erDiagram` opener.
- **`${env:VAR}` interpolation** now covers `[[logical_relation]]`
  blocks (`from`, `to`, `note`, `from_columns`, `to_columns`) so the
  multi-tenant pattern `from = "${env:SCHEMA_PREFIX}_events.user_id"`
  works the same way it does for `[[connection]]` host / database
  fields. Missing env vars surface at start-up as a clean
  `InterpolateError` instead of a confusing "unknown table" warning
  later on.
- **Workspace discovery is now cached at startup** in
  `SessionState::workspace_root`. Previously every `:diagram` call
  re-walked the file tree from `current_dir()`, so a CWD change
  (e.g. a child process chdir-ing) could silently lose the project
  boundary. The MCP server already cached its workspace; the TUI now
  matches.

### Changed

- **`:diagram <table>` subcommand parser**: tables literally named
  `export`, `impact`, or `focus` used to be unreachable through the
  muscle-memory positional form. Two new escapes resolve the
  collision: `:diagram focus <table>` spells out the implicit
  Focused-modal form, and `:diagram -- <table>` is a positional
  escape (mirrors `--` in POSIX option parsing). The bare
  `:diagram users` form still works for every other name.

### Added

- **ER diagrams (v1.2)**: schema-diagram support spanning TUI, CLI
  export and MCP. A new headless `narwhal-diagram` crate builds a
  `DiagramModel` from `TableSchema` slices and renders Mermaid
  (`erDiagram`) or Graphviz `dot`. Cardinality is computed from FK
  nullability and uniqueness (1-to-many → `||--o{`, nullable FK →
  `|o--o{`, FK with UNIQUE → `||--||`); junction tables fall out
  naturally as two 1-to-many edges. Cross-schema FKs are dropped in
  V1 so renderers never emit dangling edges.
  - **TUI modal**: `:diagram <table>` opens *Focused* mode (centre
    table with PK/FK/UK markers + 1-hop FK neighbour list).
    `:diagram impact <table>` opens *Impact* mode (reverse-FK tree
    with `ON DELETE` annotations; a warning glyph flags `NO ACTION`
    references that would block a delete). Keys inside the modal:
    `Tab`/`Shift-Tab` cycle neighbours, `Enter` re-centres on the
    selected one (instant — the model is cached), `i` toggles
    Focused↔Impact, `y` yanks the current subset as Mermaid to the
    clipboard, `q`/`Esc` close.
  - **Sidebar shortcut**: `gd` (vim-style chord) or `D` opens the
    Focused modal on the highlighted table.
  - **Export command**: `:diagram export mermaid|dot [path]`
    — with no path the rendered source is copied to the system
    clipboard, with a path it goes to disk (extension added if
    omitted). `--table T` restricts to a 1-hop focused subset;
    `--schema S` restricts candidates before the describe round-trips
    fire. Aliases: `:diag`, `mmd`, `gv`, `graphviz`.
  - **MCP tool**: `get_diagram` lets agents render the same
    diagrams. Returns a JSON envelope with node/edge counts plus
    the rendered `source`. Qualified `schema.name` targets override
    the `schema` argument; bare names consult `schema` as a hint.
    Body goes through the 512 KiB `cap_response` like every other
    tool.
  - **Config**: `[diagram] icons = "ascii" | "nerdfont"`. Default
    `ascii` keeps the modal safe in stock terminals; Nerd Font
    glyphs (key, link, star, warning) are opt-in. Mermaid / DOT
    exports always use ASCII because their downstream viewers
    (mermaid.live, Graphviz HTML labels) don't reliably ship Nerd
    Font glyphs.
- **User-declared logical relations (v1.2)**: micro-service splits
  and sharded schemas often leave behind "this column points at
  that one" relationships the engine cannot enforce. Declare them
  in `.narwhal/workspace.toml` (preferred — git-commit for your
  team) or `connections.toml` (personal fallback) and they render
  alongside the real FKs in every surface:
  - Dashed `..` notation in Mermaid (`}o..||`, etc.) and
    `style=dashed, color="#888888"` in Graphviz so logical edges
    read as informational at a glance.
  - `[L]` prefix + dashed unicode arrows (`╌╌▷` / `◁╌╌`) and
    muted styling in the TUI modal, with the user note shown as
    `↳ note` below the row.
  - Six cardinality tokens including the FK-less
    `many-to-one` (default) and `many-to-many` variants.
  - Workspace + connections-file merge with workspace winning on
    duplicates; bad entries (unknown table/column, unknown
    cardinality, composite-in-v1) are dropped with a logged
    warning instead of failing the whole diagram.
  - `narwhal_diagram::build_with_logical` returns
    `(model, diagnostics)` so MCP and TUI hosts share the same
    validation surface.

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


