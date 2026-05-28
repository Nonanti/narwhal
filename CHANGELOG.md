# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Removed

- Stale legacy per-driver crate directories
  (`crates/narwhal-driver-{postgres,mysql,sqlite,duckdb,clickhouse,registry}/`).
  These were never workspace members and had diverged from the
  canonical sources under `crates/narwhal-drivers/`. No user-visible
  impact.

### Docs

- `ARCHITECTURE.md`, `EXCEPTIONS.md`, `RELEASING.md`,
  `dev/async-trait-style.md`, `CONTRIBUTING.md`, and the
  `narwhal-mcp` README now reflect the consolidated
  `narwhal-drivers` layout.

## [2.2.0] - 2026-06-08

### Security / Breaking

- **`LuaSandbox::default()` is now `Restricted`** (previously
  `Permissive`). Scripts loaded via `LuaPlugin::from_script` or
  `LuaPlugin::from_path` run in a VM without `io`, `os`, `package`,
  `debug`, or `ffi`.

  Trusted scripts that need that surface (e.g. the shipped
  `examples/plugins/csv_export.lua`, which writes via `io.open`)
  must opt in through the new `LuaPlugin::from_path_with_sandbox`
  / `LuaPlugin::from_script_with_sandbox` constructors with
  `LuaSandbox::Permissive`.

  This is the reason 2.2.0 is a minor release rather than a patch.

  Migration: if you embed `narwhal-plugin-lua` and load plugins
  you trust to native-code level, replace
  `LuaPlugin::from_path(path)` with
  `LuaPlugin::from_path_with_sandbox(path, LuaSandbox::Permissive)`.
  If you load user-installed plugins, keep the default — untrusted
  scripts can no longer reach the filesystem, spawn processes, or
  escape via `debug.getregistry`.

  The `narwhaldb` binary's plugin auto-loader uses the safe
  default. User plugins from `~/.config/narwhal/plugins/` that
  depend on `io` / `os` now load but fail at dispatch with
  `attempt to index a nil value (global 'io')`. A future release
  will surface this as a per-plugin manifest flag.

### Fixed — event dispatch & editor

- Mouse clicks outside the editor pane while vim is in Command
  mode no longer strand the `:` prompt. Each click handler now
  cancels Command mode before routing to per-pane handlers.
- `Ctrl-W` keyboard focus cycling gets the same protection.
- Mouse events no longer mutate background state while a modal is
  open. Clicks on the sidebar or editor used to change focus,
  move the cursor, or fire a sidebar preview query underneath
  the modal.
- Multi-byte editor clicks (Turkish, CJK, accented Latin) now walk
  by display width instead of byte length, so clicks land on a
  valid grapheme boundary and never produce a UTF-8
  continuation-byte cursor that downstream slicing would panic on.
  Double-click word selection respects grapheme boundaries.
- Pending key leaders are cleared on every mouse event. A click
  after `]r`, `g`, or emacs `C-x` no longer swallows the next
  keystroke.
- Keybinding presets no longer steal emacs / basic editor chords.
  VSCode `C-p`, DataGrip / IntelliJ `C-b`, and `C-Shift-P` now
  fall through to the per-pane handler when the editor is in
  basic or emacs mode.

### Fixed — settings live-reload

- `apply_settings` rebuilds the keymap from builtin defaults on
  every apply. Removed overrides no longer linger until restart.
- `keymap_warnings` is cleared on every apply. A malformed binding
  no longer accumulates one duplicate warning per file write.
- Editor mode switch resets the vim state machine. A user stuck in
  Command mode no longer stays there after a runtime switch to
  basic or emacs.
- The settings watcher filters events by file name. Sibling writes
  (`connections.toml`, `workspace-state.toml`, `sessions/*.toml`)
  no longer trigger a full settings reload, keymap rebuild, and
  stream-tuning resend.
- External edits to `config.toml` while `:settings` is open now
  cleanly close the stale modal with a status-bar message,
  instead of overwriting the external edit on the next Ctrl-S.

### Fixed — correctness & robustness

- SQLite driver now sets `PRAGMA foreign_keys = ON` and
  `busy_timeout = 5s` at connect time. `REFERENCES ... ON DELETE
  CASCADE` declarations are now actually enforced; concurrent
  reader/writer collisions retry for 5 s instead of failing
  instantly.
- `guard_read_only` no longer rejects safe `SELECT`s with sleep
  keywords inside comments. Line and block comment bodies are
  now mask-replaced before the denylist scan, matching the
  handling of string literals. MCP `run_query` accepts annotated
  SQL again.
- Context menu render no longer panics on narrow terminals
  (`u16::clamp(12, screen.width - 2)` could panic with `min > max`
  on terminals under 14 cells).
- MySQL `SslMode::VerifyCa` now skips hostname validation while
  still verifying the CA chain, matching the Postgres driver's
  `verify_ca_client_config` semantics. VerifyCa users no longer
  hit surprising hostname failures on wildcard or load-balancer
  certs.
- Audit file sink rejects invalid `strftime` tokens at startup
  instead of panicking on the first event. A stray `%` in the
  path template now surfaces as `SinkError::Path`.
- LSP cancel is now fire-and-forget (`try_send`). A saturated
  outbound channel can no longer block the timeout cleanup that
  triggered the cancel.
- LSP response routing normalises `Id::String`. Proxy / replay
  layers that rewrite ids to strings now round-trip correctly;
  string-id responses were previously dropped.
- Connection wizard, history modal, and editor search input
  fields no longer absorb `Ctrl`-modified `Char` keystrokes.
  Ctrl-C / Ctrl-V / Ctrl-A are now handled by the upper chord
  layer instead of inserting literal `c` / `v` / `a` into the
  input.
- Tab switch resets vim state to Normal mode. Transient modes
  (Visual, Command, Insert, OperatorPending) on tab 1 no longer
  leak into tab 2.
- `editor.mouse = disabled` now blocks all mouse paths. The guard
  previously only covered editor-body selection; right-click,
  middle-click paste, scroll, and pane-focus changes still
  fired.
- `AggKind::Count` rustdoc clarified as `COUNT(*)` semantics.
  Rows where the value column is NULL are still counted.

## [2.1.0] - 2026-06-08

### Distribution

- One-line installer:
  ```
  curl -fsSL https://github.com/Nonanti/narwhal/releases/latest/download/install.sh | sh
  ```
  Detects OS and architecture, verifies the SHA-256, and drops the
  binary into `~/.local/bin`. Honours `NARWHAL_VERSION`,
  `NARWHAL_BIN_DIR`, and `NARWHAL_FORCE`.
- Linux prebuilt binaries now vendor libdbus statically. They run on
  minimal containers, Alpine, and NixOS without
  `libdbus-1.so.3` installed on the host.

### Added — editor customization

- **Three editor modes** — `[editor].mode` picks between `vim` (the
  v1.x behaviour, still default), `basic` (modeless IDE-style), and
  `emacs` (Ctrl- / Meta- chords with a `C-x` prefix). Switch at
  runtime with `:mode vim|basic|emacs`; the change is persisted to
  `config.toml`. A v1.x file with `keybindings.vim_mode = false` is
  interpreted as `editor.mode = "basic"` at runtime — no migration
  pass needed. See `docs/editor-modes.md`.
- **Mouse support** — click positions the cursor, drag extends a
  selection, double-click selects the word, triple-click selects
  the line, middle-click pastes, right-click opens the editor
  context menu (Cut / Copy / Paste / Select All / Run Selection /
  Find / Toggle Comment). Configurable via
  `[editor].mouse = enabled | click-only | disabled`. See
  `docs/mouse.md`.
- **`:settings` modal** — in-app editor for editor mode, mouse
  mode, theme, line numbers, mode indicator, auto-indent,
  current-line highlight, word wrap, and the keybinding preset.
  Atomic save to `config.toml`; the footer flips colour to flag
  unsaved changes.
- **Keybinding presets** — `[keybindings].preset = default | vscode
  | datagrip | intellij` layers a small set of IDE chords on top
  of the built-in defaults. VSCode adds `Ctrl+P` (goto) and
  `Ctrl+Shift+P` (command palette); DataGrip / IntelliJ add
  `Ctrl+B` (focus sidebar) and `Ctrl+Enter` (run statement).
  User `[keymap.*]` overrides still win.
- **Selection + undo plumbing** — `narwhal_domain::editor` gains
  `Selection`, `SelectionKind`, `EditHistory`, `EditOp`,
  `BufferSnapshot`. `EditorBuffer` exposes `selection()`,
  `set_selection()`, `extend_selection_to()`, `selected_text()`,
  `delete_selection()`, `snapshot()` / `commit_undo_snapshot()`,
  `undo()` / `redo()` so every editor mode (and the mouse drag
  path) share one cut / copy / undo pipeline.
- **Mode indicator** — the status bar's left segment shows
  `BASIC` / `EMACS` (with a `C-x` flip when the emacs prefix is
  armed). Disable with `[editor].show_mode_indicator = false`.
- **Dynamic help** — `F1` swaps the *Editor* page between vim,
  basic, and emacs cheatsheets based on the active mode.
- **Live reload** — a `notify`-driven watcher reapplies any
  external edit to `config.toml` within ~50 ms. Self-writes from
  the modal are suppressed inside a 750 ms window so the in-app
  Ctrl+S does not echo through the watcher.

New docs: `docs/editor-modes.md`, `docs/mouse.md`,
`docs/settings.md`.

### Deprecated

- `[keybindings].vim_mode`. Use `[editor].mode = "basic"` (or
  `"emacs"`) instead. The old field still round-trips for
  back-compat.

## [2.0.0] - 2026-06-05

Major release. Breaking changes concentrated below; feature work
ships without semantic breaks.

### Breaking

- **MSRV bumped to 1.85**; edition 2024 throughout.
- **`Connection` trait uses native `async fn` (RPITIT)**; downstream
  drivers no longer depend on `async-trait`.
- **Driver crate consolidation**: the six per-backend crates
  collapsed into `narwhal-drivers` with one cargo feature per
  backend.
- **Settings schema v2** with the `narwhal migrate-config` helper
  to bring v1 files forward.
- **API surface audit** — several previously-public types are now
  `pub(crate)`.

### Added

- **Streaming results** (`QueryStream`) — rows arrive incrementally;
  the result pane ticks live, with a configurable batch size and
  time-window flush.
- **MSSQL driver** via `tiberius`.
- **Connection vault** (HashiCorp Vault + 1Password).
- **Treesitter SQL parser** — scope-aware completion and cursor
  context.
- **Workspace persistence** — tabs, cursor, and sidebar restore
  across launches.
- **Parquet + Markdown exporters**.
- **WASM plugin runtime** (`wasmtime` + capability sandbox v2).
- **Audit log** — append-only JSONL sink suitable for compliance
  evidence.
- **Schema diff** — `:schema-diff src tgt` and headless
  `narwhal schema-diff` emit dialect-specific migration DDL.
- **Inline ASCII charts** — `:chart bar|line|sparkline` over any
  result.
- **Pivot table** — `:pivot rows=.. cols=.. value=.. agg=..` with
  count / sum / avg / min / max aggregators.
- **Multi-cursor editing** — Alt-N / Alt-A / Esc collapses; insert
  and delete propagate across cursors. Vim block-visual interop
  and undo-as-one-step deferred to v2.1.
- **Embedded LSP client crate** (sqls / sqlls protocol
  primitives). Editor wiring deferred to v2.1.
- **Plugin-defined MCP tools** — host-side registration with
  collision policy; WIT bridge deferred to v2.1.
- New workspace members: `narwhal-audit`, `narwhal-schema-diff`,
  `narwhal-pivot`, `narwhal-lsp`, `narwhal-plugin-wasm`.
- `narwhal audit tail` CLI for the audit JSONL sink.
- `narwhal schema-diff` CLI (headless complement to the TUI
  command).
- `narwhal migrate-config` CLI.

### Fixed

- Driver errors are preserved in the `source` chain across every
  backend (Postgres, MySQL, SQLite, DuckDB, ClickHouse). Previously
  every driver flattened its engine error into a string, making
  `find_source::<T>()` impossible.
- DuckDB `read_only = true` is now enforced at connect time via
  `access_mode = READ_ONLY`. The previous code silently produced a
  writable connection.
- ClickHouse IPv6 hosts are now bracketed per RFC 3986
  (`https://[::1]:8123/`).
- SQLite and DuckDB `close()` drop the connection handle explicitly
  via `Option::take()` instead of waiting for the `Arc` refcount to
  reach zero. Releases the SQLite file lock immediately.
- MySQL and ClickHouse: `ssl_cert` set without `ssl_key` (or vice
  versa) is now rejected at config time instead of silently
  falling through to a non-mTLS connection.
- History redaction regex now covers `mssql://` and `sqlserver://`
  DSNs, plugging a password-leak path into the journal.

### Changed

- `CredentialStore` migrated from `#[async_trait]` to native
  `async fn` in trait (RPITIT), matching the `Connection` trait
  pattern. Added a `DynCredentialStore` blanket-impl sibling. The
  `async-trait` dependency is dropped.
- `InMemoryStore` uses `parking_lot::Mutex` (unpoisonable),
  removing the "lock poisoned" error path entirely.
- `LogicalRelationConfig` no longer has
  `#[serde(deny_unknown_fields)]` for forward compatibility.
- `LogicalRelation`, `Cardinality`, and `QualifiedName` moved from
  `narwhal-diagram` to `narwhal-domain`. Re-exported from
  `narwhal-diagram` for backward compatibility. Removes an
  inverted dependency (`narwhal-config` → `narwhal-diagram`).
- Parquet exporter emits `tracing::warn!` when the type-inference
  window misses a value (previously a silent NULL drop).
- MySQL schema-diff emitter no longer produces invalid SQL for
  nullable-only or default-only changes (previously embedded
  `/* keep existing type */` mid-statement). Added the `int → int4`
  synonym, precision-qualified timestamp normalisation, and
  `::type` cast suffix stripping in defaults. The `:diff` command
  now uses the same canonical comparison as
  `narwhal-schema-diff`.
- Vim mode gains `gg` (file start), visual-mode count prefix
  (`3j` extends the selection), and operator wiring for yank /
  delete / change (`dd`, `yy`, `dw`, `dgg`, `cc`).
- Audit log rotation suffix is now a millisecond timestamp
  (`audit.log.20260604T143012.473Z`), with a numeric `-N` suffix
  on sub-millisecond collision.
- `ToolDescriptor`: `name` and `description` are `Cow<'static,
  str>` for zero-alloc round-trips on built-in tools. JSON wire
  shape unchanged.
- `LspError::Timeout` carries the method name, so status-bar
  surfacing reads `"LSP request 'textDocument/hover' timed out"`
  instead of a generic message.

## [1.2.0] - 2026-06-02

### Added

- **ER diagrams**: schema-diagram support spanning TUI, CLI export,
  and MCP. The new headless `narwhal-diagram` crate builds a
  `DiagramModel` from `TableSchema` slices and renders Mermaid
  (`erDiagram`) or Graphviz `dot`. Cardinality is computed from
  FK nullability and uniqueness (1-to-many → `||--o{`, nullable
  FK → `|o--o{`, FK with UNIQUE → `||--||`); junction tables fall
  out naturally as two 1-to-many edges. Cross-schema FKs are
  dropped so renderers never emit dangling edges.
  - **TUI modal**: `:diagram <table>` opens *Focused* mode (centre
    table with PK / FK / UK markers + 1-hop FK neighbour list).
    `:diagram impact <table>` opens *Impact* mode (reverse-FK tree
    with `ON DELETE` annotations; a warning glyph flags `NO
    ACTION` references that would block a delete). Inside the
    modal: `Tab` / `Shift-Tab` cycle neighbours, `Enter` re-centres,
    `i` toggles Focused ↔ Impact, `y` yanks the current subset as
    Mermaid to the clipboard, `q` / `Esc` close.
  - **Sidebar shortcut**: `gd` or `D` opens the Focused modal on
    the highlighted table.
  - **Export command**: `:diagram export mermaid|dot [path]` — no
    path copies to clipboard; with a path it goes to disk
    (extension added if omitted). `--table T` restricts to a
    1-hop focused subset; `--schema S` restricts candidates
    before the describe round-trips fire. Aliases: `:diag`,
    `mmd`, `gv`, `graphviz`.
  - **MCP tool**: `get_diagram` lets agents render the same
    diagrams. Returns a JSON envelope with node and edge counts
    plus the rendered `source`. Qualified `schema.name` targets
    override the `schema` argument; bare names consult `schema`
    as a hint. Body goes through the 512 KiB `cap_response`.
  - **Config**: `[diagram] icons = "ascii" | "nerdfont"`. Default
    `ascii` keeps the modal safe in stock terminals; Nerd Font
    glyphs (key, link, star, warning) are opt-in. Mermaid / DOT
    exports always use ASCII because their downstream viewers
    don't reliably ship Nerd Font glyphs.
- **User-declared logical relations**: micro-service splits and
  sharded schemas often leave behind "this column points at that
  one" relationships the engine cannot enforce. Declare them in
  `.narwhal/workspace.toml` (preferred — git-commit for your
  team) or `connections.toml` (personal fallback) and they render
  alongside real FKs in every surface:
  - Dashed `..` notation in Mermaid (`}o..||`, etc.) and
    `style=dashed, color="#888888"` in Graphviz so logical edges
    read as informational at a glance.
  - `[L]` prefix + dashed unicode arrows (`╌╌▷` / `◁╌╌`) and
    muted styling in the TUI modal, with the user note shown as
    `↳ note` below the row.
  - Six cardinality tokens including the FK-less
    `many-to-one` (default) and `many-to-many` variants.
  - Workspace + connections-file merge with workspace winning on
    duplicates. Bad entries (unknown table or column, unknown
    cardinality, composite-in-v1) are dropped with a logged
    warning instead of failing the whole diagram.
  - `narwhal_diagram::build_with_logical` returns
    `(model, diagnostics)` so MCP and TUI hosts share the same
    validation surface.

### Fixed

- Diagram renderers now escape control characters in edge labels.
  A column or table identifier containing a literal `\n` (legal
  in PostgreSQL via quoted identifiers) used to break Mermaid
  parsing with an "unexpected token" and mangle DOT edges.
  Mermaid downgrades newlines and tabs to spaces; DOT escapes
  them as the literal `\n` / `\r` / `\t` glyphs the Graphviz
  parser expects.
- Mermaid title sanitiser strips the `---` token so a title
  containing the YAML front-matter delimiter cannot close the
  block early and inject a bogus `erDiagram` opener.
- `${env:VAR}` interpolation now covers `[[logical_relation]]`
  blocks (`from`, `to`, `note`, `from_columns`, `to_columns`)
  so the multi-tenant pattern
  `from = "${env:SCHEMA_PREFIX}_events.user_id"` works the same
  way it does for `[[connection]]` host and database fields.
- Workspace discovery is cached at startup in
  `SessionState::workspace_root`. Previously every `:diagram`
  call re-walked the file tree from `current_dir()`, so a CWD
  change could silently lose the project boundary.

### Changed

- `:diagram <table>` subcommand parser: tables literally named
  `export`, `impact`, or `focus` used to be unreachable through
  the muscle-memory positional form. Two new escapes resolve the
  collision: `:diagram focus <table>` spells out the implicit
  Focused-modal form, and `:diagram -- <table>` is a positional
  escape (mirrors `--` in POSIX option parsing). The bare
  `:diagram users` form still works for every other name.

## [1.1.0] - 2026-05-29

### Added

- Optional `color`, `confirm_writes`, and `read_only` fields on
  `[[connection]]`. The active connection's name is tinted in the
  status bar; writes to confirm-marked connections require typing
  `YES`; read-only connections reject non-SELECT batches at the
  syntactic guard and via `set_read_only(true)` on the driver
  session.
- **`:goto` fuzzy navigator** — Ctrl-N / `:goto` / `:g` opens a
  Helix-style fuzzy matcher over every schema, table, and view
  across all open sessions. Handles non-ASCII identifiers
  (Turkish, Cyrillic, CJK) correctly.
- **Explain tree visualiser** — cost bars and hot-path colouring
  for `EXPLAIN` output.
- **`:submit` / `:revert`** — command aliases to flush or discard
  the pending-mutation queue.
- **Foreign-key navigation** — `f` (or `gd`) in the results pane
  on a foreign-key cell opens a new SELECT scoped to the
  referenced row. Identifiers are dialect-quoted and the cell
  value is bound as a query parameter — no string interpolation.
- **Result palette filters** — `:filter <expr|clear>` and
  `:sort <N|clear>` expose the in-memory filter/sort layer through
  the command palette.
- **Schema diff migration generator** — `:diff <a> <b>` compares
  two connections and emits ALTER TABLE statements.
- **SQL linter** — `:lint` flags `SELECT *`, `UPDATE` / `DELETE`
  without `WHERE`, `TRUNCATE`, and FROM-comma Cartesian joins.
  The destructive-no-where rule goes through the statement
  splitter, so a `;` inside a string literal no longer fragments
  the source into a false-positive UPDATE.
- **Templates and history search** — `:tpl` inserts built-in
  templates (sel / ins / upd / del / join / with);
  `:history [pattern]` opens a pre-filtered Ctrl-R modal.
- `ConnectionParams::with(|p| { ... })` builder helper so callers
  outside `narwhal-core` can construct the struct without
  struct-literal syntax (it is now `#[non_exhaustive]`).

### Changed

- `ConnectionParams` is marked `#[non_exhaustive]`. Future field
  additions stay non-breaking. Migration: replace
  `ConnectionParams { ..Default::default() }` with
  `ConnectionParams::with(|p| { ... })`.
- `RunRequest` now carries a `params_per_statement` vector and
  exposes `RunRequest::new` / `RunRequest::with_params` so
  internal callers (foreign-key nav, future programmatic
  dispatch) can route bound parameters end-to-end through
  `spawn_run`.
- Cargo description: "Multi-driver TUI database client with a
  built-in MCP server."

### Fixed

- Goto fuzzy navigator no longer panics on non-ASCII table names.
  The previous `Utf32Str::Ascii(s.as_bytes())` shortcut
  interpreted UTF-8 bytes as ASCII code units.
- Foreign-key navigation is no longer vulnerable to SQL injection
  through the cell value or through unusual identifier
  characters. Identifiers are dialect-quoted; the value is bound
  as a query parameter.
- Ctrl-N inside an open completion popup advances the popup
  (mirroring vim / IDE convention) instead of stealing focus to
  the `:goto` modal. Ctrl-P added as the inverse.
- The lint rule for destructive-without-WHERE no longer splits
  the source on every `;`. Statements containing literal
  semicolons in string literals are kept whole, eliminating
  false positives and missed cases.

## [1.0.0] - 2026-05-24

First public release.

### Added

- Row CRUD with a pending-changes pipeline. `o` queues an empty
  insert, `O` duplicates the focused row, `d` queues a delete;
  cell edit (`e` + `Enter`) queues an `UPDATE` instead of hitting
  the database. `Ctrl-S` commits every staged mutation in a
  single transaction (or savepoint when the user is already
  inside one); `Ctrl-X` discards the queue; `Ctrl-P` (or
  `:pending` / `:diff`) toggles a preview modal of the generated
  SQL. Optimistic concurrency is encoded into every WHERE clause
  so a concurrent edit fails the commit instead of silently
  overwriting. Primary-key guard refuses `d` on tables without a
  PK.
- Metadata tabs in TableDetail. Sidebar `Enter` on a table opens
  a five-tab view: Records · Columns · Constraints · Foreign
  Keys · Indexes. `1`–`5` switches the active tab.
- Built-in JSON viewer. `z` opens the focused cell in a
  full-screen modal, `Z` opens the whole row.
  `j/k/Ctrl-D/Ctrl-U/g/G` scroll, `y/Y` yank, `q` / `Esc` close.
  Pretty-prints valid JSON via `serde_json` and falls back to
  the raw payload otherwise.
- Action + Keymap layer. New `narwhal_commands::action` (Action
  enum + KeyGroup taxonomy) and `narwhal_commands::keymap`
  (registry + chord parser). `[keymap.<group>]` overrides in
  `config.toml` rebind any chord; malformed entries surface as
  warnings instead of panicking.
- History modal enrichment. Ctrl-R now shows an outcome glyph
  (● green / yellow / red), elapsed timing (auto-scaled
  ms/s/m), and rows summary (↓N for returned, ∼N for affected).
- `${env:VAR}` interpolation. Connection params, SSH config, and
  SSL certificate paths accept `${env:NAME}` and
  `${env:NAME:fallback}` placeholders. Fallbacks may themselves
  be `${env:...}` references up to depth 8. Missing variables
  surface as `ConfigError::Interpolate`.
- Pre-connect commands. Each connection can carry an ordered
  list of `[[connections.pre_connect]]` shell steps that run
  before the SSH tunnel and the driver. Each step's stdout can
  be captured into a named variable (`save_output_to`) and
  exposed via `${preconnect:NAME}`. Per-step `timeout_secs`
  (default 30) and `required` (default true) flags bound
  execution.
- `--read-only` flag. Refuses every row-level mutation regardless
  of the driver's `row_level_dml` capability. The TUI shows an
  `[RO]` badge; `exec` refuses `--write` while `--read-only` is
  in effect.
- Pending mutations badge. Status bar shows `⏳N pending` whenever
  the staged-mutation queue is non-empty.
- Audit log entries for pending commits. Each committed mutation
  lands in the journal as a separate `HistoryEntry` tagged
  `source = "pending"`.
- `row_level_dml` capability flag. Postgres / SQLite / MySQL /
  DuckDB opt in; ClickHouse declines and the row CRUD pipeline
  refuses staging with engine-specific guidance.
- `:pending` / `:diff` command palette entry for the `Ctrl-P`
  chord.
- `SECURITY.md` with private disclosure policy, scope, and
  hardening notes.
- `CONTRIBUTING.md` covering workflow, commit conventions, code
  style, and the per-PR checklist.
- `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1).
- GitHub issue templates (bug report, feature request) and a PR
  template.
- `dependabot.yml` for weekly cargo + GitHub Actions updates.
- `docs/img/demo.gif` recorded with VHS and reproducible from
  `docs/img/demo.tape` + `docs/img/seed-demo-db.sh`.

### Changed

- README tagline leads with the built-in MCP server: "A TUI
  database client with a built-in MCP server. Five databases,
  vim editing, Lua plugins." The MCP section sits next to Quick
  Start so it lands above the fold.
- Install section lists Cargo / cargo-binstall / Homebrew tap /
  AUR / Nix as first-class options.
- `packaging/homebrew/narwhal.rb` dropped runtime `postgresql` /
  `mysql-client` dependencies (drivers link statically); kept
  only `rust` + `cmake` + `llvm` as build deps. Now uses
  `std_cargo_args`.
- `packaging/aur/PKGBUILD` switched to the standard `prepare /
  build / check / package` layout, ships both LICENSE-MIT and
  LICENSE-APACHE, installs the README under `share/doc/`.
- The binary crate is published as `narwhaldb` on crates.io. The
  bare `narwhal` slot was squatted in 2018 by an abandoned
  docker library. The installed command name is unchanged (still
  `narwhal`); only the install incantation differs:
  `cargo install narwhaldb` instead of `cargo install narwhal`.
- Release tarball naming uses `narwhal-X.Y.Z-<target>`.

### Architecture refactor

The workspace was reorganised around a strict view / domain / app /
driver split. No user-facing behaviour changes; the binary's CLI,
keymap, config schema, and MCP protocol are unchanged.

- New `narwhal-driver-registry` crate — single home for the
  `DriverRegistry` previously duplicated in `narwhal-app` and
  `narwhal-mcp`. Bundled drivers are opt-in via cargo features
  (`driver-postgres`, `driver-sqlite`, `driver-mysql`,
  `driver-duckdb`, `driver-clickhouse`, `all-drivers`).
- New `narwhal-domain` crate — pure model state with no IO and
  no rendering. Initial residents: `EditorBuffer` and its
  support types, the `SchemaListing` type alias.
- New `narwhal-commands` crate — stateless command and helper
  modules: command dispatch, completion engine, export pipeline,
  connection wizard, snippet store, DDL/EXPLAIN helpers, inline
  cell edit, statement extraction, meta queries, session types.
- Build matrix. The binary defaults to `["driver-postgres",
  "driver-sqlite"]`; downstream packagers can pick the exact
  driver set they want.
  `cargo build -p narwhal --no-default-features --features
  driver-sqlite` produces a minimal SQLite-only build.
- `docs/ARCHITECTURE.md` — layer diagram, state ownership table,
  dependency rules, feature matrix.
- `docs/STYLE.md` — code style: file and function size limits,
  lint allow-list, error / async / logging rules.
- `narwhal-app` shrunk from 12 391 LOC to 6 909 LOC (−44 %).
- `AppCore` god-struct cracked open. `core/mod.rs` went from
  1 498 LOC to 150 LOC. State types moved to
  `core/state/{result, tab, sidebar, history, snippets_modal,
  status}`.
- `editor_dispatch.rs` (1 066 LOC) split into a directory:
  `mod.rs`, `editor_keys.rs`, `search.rs`, `completion.rs`,
  `sidebar.rs`.
- `wizard.rs` (930 LOC) split into a directory: `mod.rs`,
  `fields.rs`, `state.rs`, `logic.rs`, `path.rs`.
- `narwhal-plugin-lua` no longer depends on `narwhal-core`
  directly. Plugin runtimes consume the narrow surface exported
  by `narwhal-plugin`.
- `narwhal-tui` split. `widgets/editor.rs` (1 041 LOC) is now
  341 LOC — only render code. The text buffer model lives in
  `narwhal-domain::editor`. `widgets/results.rs` (1 301 LOC)
  became a module with seven files.
- `narwhal-commands` module split.
  `export.rs` (1 332 LOC) →
  `export/{csv,json,tsv,table,insert,quoting,source,format,error}`.
  `completion.rs` (1 041 LOC) →
  `completion/{context,tokenizer,items,keywords,gather}`.
- Workspace lints upgraded to `clippy::pedantic` +
  `clippy::nursery` with a documented allow-list. The build
  passes under `cargo clippy --workspace --all-targets -- -D
  warnings` with zero warnings.
- Workspace formatted with `cargo fmt --all`; the CI step
  `cargo fmt --check` passes.
- Rustdoc passes under `RUSTDOCFLAGS='-D warnings'`.
- File renames that fixed a long-standing naming collision:
  `narwhal-app/src/edit.rs` → `cell_edit.rs`, `editor.rs` →
  `statements.rs`, `core/editor_handlers.rs` →
  `core/editor_dispatch.rs`.
- 120 banner comment lines removed across the workspace.
- `#[non_exhaustive]` removed from workspace-internal enums that
  now cross crate boundaries (these are internal types, not
  public API).
