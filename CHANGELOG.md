# Changelog

All notable changes to this project are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [SemVer](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [2.2.0] - 2026-06-08

### Security

- **`LuaSandbox::default()` is now `Restricted`** (previously
  `Permissive`). Lua plugins no longer have access to `io`, `os`,
  `package`, `debug`, or `ffi` by default. Embedders that need the
  old behaviour must opt in via `LuaPlugin::from_path_with_sandbox`
  with `LuaSandbox::Permissive`. Auto-loaded plugins from
  `~/.config/narwhal/plugins/` use the safe default.

  Listed as breaking because the default behaviour changed.
  See [upgrading guide](./docs/upgrading.md#to-22x).

### Fixed

- Mouse clicks no longer strand the `:` command prompt when vim is
  in Command mode. The same fix applies to `Ctrl-W` focus cycling.
- Mouse events no longer mutate background state while a modal is
  open.
- Multi-byte editor clicks (Turkish, CJK, accented Latin) walk by
  display width instead of byte length. Clicks now land on valid
  grapheme boundaries.
- Pending key leaders (`]r`, `g`, emacs `C-x`) are cleared on every
  mouse event.
- Tab switch resets vim state to Normal mode. Transient modes on
  one tab no longer leak into the next.
- `editor.mouse = disabled` blocks all mouse paths, including
  scroll and pane focus changes.
- Settings live-reload now rebuilds the keymap from defaults on
  every apply, and clears stale warnings.
- The settings watcher filters events by file name; sibling writes
  (`connections.toml`, `workspace-state.toml`) no longer trigger a
  full settings reload.
- External edits to `config.toml` while `:settings` is open close
  the modal with a status-bar message instead of overwriting the
  edit on the next save.
- SQLite driver enables `PRAGMA foreign_keys = ON` and
  `busy_timeout = 5s` at connect time.
- `guard_read_only` no longer rejects safe `SELECT`s with
  destructive keywords inside comments. MCP `run_query` accepts
  annotated SQL again.
- Context menu render no longer panics on terminals under 14 cells
  wide.
- MySQL `SslMode::VerifyCa` skips hostname validation while still
  verifying the CA chain.
- Audit file sink rejects invalid `strftime` tokens at startup
  instead of panicking on the first event.
- LSP cancel is fire-and-forget; a saturated outbound channel can
  no longer block the cleanup that triggered it.
- LSP response routing handles `Id::String` round-trips.
- Input modal fields (wizard, history search) no longer absorb
  `Ctrl`-modified `Char` keystrokes.

## [2.1.0] - 2026-06-08

### Added

- One-line installer:
  `curl -fsSL https://github.com/Nonanti/narwhal/releases/latest/download/install.sh | sh`.
  Detects the platform, verifies the SHA-256 sum, and installs into
  `~/.local/bin`. Honours `NARWHAL_VERSION`, `NARWHAL_BIN_DIR`, and
  `NARWHAL_FORCE`.
- Linux prebuilt binaries vendor libdbus statically — they run on
  Alpine, minimal containers, and NixOS without
  `libdbus-1.so.3`.
- **Three editor modes**: `vim` (default), `basic` (modeless), and
  `emacs`. Switch at runtime with `:mode`. See
  [editor modes](./docs/editor-modes.md).
- **Mouse support** across editor, results, sidebar, and tabs:
  click, drag, double / triple click, middle-click paste,
  right-click menus. Configurable via `[editor].mouse`. See
  [mouse](./docs/mouse.md).
- **`:settings` modal** — in-app editor for the common settings.
  Atomic save, live reload on external edits.
- **Keybinding presets**: `default`, `vscode`, `datagrip`,
  `intellij`. User overrides still win.
- **Selection and undo plumbing**: shared across all three editor
  modes and the mouse drag path.

### Deprecated

- `[keybindings].vim_mode`. Use `[editor].mode = "basic"` instead.
  The old field still round-trips for back-compat.

## [2.0.0] - 2026-06-05

Major release. Plan a configuration migration window.

### Breaking

- MSRV bumped to **Rust 1.85**; edition 2024 throughout.
- `Connection` trait uses native `async fn` (RPITIT); downstream
  drivers no longer need `#[async_trait]`.
- The six per-backend driver crates collapsed into
  `narwhal-drivers` with one cargo feature per backend.
- Settings schema v2. Run `narwhal migrate-config` to bring v1
  files forward.
- API surface audit — several previously public types are now
  `pub(crate)`. See the [upgrading guide](./docs/upgrading.md#to-20x).

### Added

- **Streaming results** (`QueryStream`) — rows arrive incrementally.
- **MSSQL driver** via `tiberius`.
- **Connection vault**: HashiCorp Vault, 1Password CLI, AWS
  Secrets Manager, Azure Key Vault.
- **Tree-sitter SQL parser** for scope-aware completion and cursor
  context.
- **Workspace persistence** — open tabs, cursor positions, and
  sidebar expansion restore across launches.
- **Parquet and Markdown exporters**.
- **WASM plugin runtime** with a capability sandbox.
- **Audit log** — append-only JSONL, multiple sinks.
- **Schema diff** — `:schema-diff` in the TUI and
  `narwhal schema-diff` headless.
- **Inline ASCII charts**: `:chart bar|line|sparkline`.
- **Pivot table**: `:pivot rows=... cols=... value=... agg=...`.
- **Multi-cursor editing**: Alt-N / Alt-A / Esc collapse.
- **Embedded LSP client crate** (sqls / sqlls primitives).
- New CLI verbs: `narwhal audit tail`, `narwhal schema-diff`,
  `narwhal migrate-config`.

### Fixed

- Driver errors now preserve the `source` chain across every
  backend. `find_source::<T>()` works on every driver error.
- DuckDB `read_only = true` enforced at connect time
  (`access_mode = READ_ONLY`).
- ClickHouse IPv6 hosts are bracketed per RFC 3986
  (`https://[::1]:8123/`).
- SQLite and DuckDB `close()` drop the connection handle
  explicitly. The SQLite file lock releases immediately.
- MySQL and ClickHouse: `ssl_cert` without `ssl_key` is rejected
  at config time instead of falling through to a non-mTLS
  connection.
- History redaction covers `mssql://` and `sqlserver://` DSNs.

## [1.2.0] - 2026-06-02

### Added

- **ER diagrams**: Mermaid (`erDiagram`) and Graphviz DOT
  renderers. TUI modal (`:diagram <table>`), CLI export, and an
  MCP `get_diagram` tool. Cardinality is derived from FK
  nullability and uniqueness. Cross-schema FKs are dropped so
  renderers never emit dangling edges.
- **User-declared logical relations**: `.narwhal/workspace.toml`
  (preferred) or `connections.toml` accepts
  `[[logical_relation]]` entries that render alongside real FKs
  with dashed-edge styling.
- `[diagram] icons = "ascii" | "nerdfont"` config knob.

### Fixed

- Diagram renderers escape control characters in identifiers
  (column names with literal `\n` no longer break Mermaid).
- Mermaid title sanitiser strips `---` to prevent
  YAML-front-matter injection.
- `${env:VAR}` interpolation covers `[[logical_relation]]` fields.
- Workspace discovery is cached at startup so a CWD change can't
  lose the project boundary mid-session.

### Changed

- `:diagram <table>` accepts `:diagram focus <table>` and
  `:diagram -- <table>` escapes for tables literally named
  `export`, `impact`, or `focus`.

## [1.1.0] - 2026-05-29

### Added

- Optional `color`, `confirm_writes`, and `read_only` fields on
  `[[connection]]`. Read-only connections refuse non-SELECT
  batches at the syntactic guard.
- `:goto` (`Ctrl-N`) — Helix-style fuzzy navigator over every
  schema, table, and view. Handles non-ASCII identifiers
  (Turkish, Cyrillic, CJK).
- `EXPLAIN` tree visualiser with cost bars and hot-path colouring.
- `:submit` / `:revert` to flush or discard the pending-mutation
  queue.
- Foreign-key navigation: `f` (or `gd`) on an FK cell opens a
  scoped SELECT. Identifiers are dialect-quoted; values are bound
  as parameters.
- `:filter <expr|clear>` and `:sort <N|clear>` palette commands.
- `:diff <a> <b>` — schema diff between two connections (the
  v2.0 `:schema-diff` predecessor).
- `:lint` — flags `SELECT *`, `UPDATE` / `DELETE` without
  `WHERE`, `TRUNCATE`, and FROM-comma Cartesian joins.
- `:tpl` snippet inserts and `:history [pattern]` filtered
  Ctrl-R modal.
- `ConnectionParams::with(|p| { ... })` builder helper.

### Changed

- `ConnectionParams` is `#[non_exhaustive]`. Future field
  additions stay non-breaking.

### Fixed

- `:goto` no longer panics on non-ASCII table names.
- Foreign-key navigation is no longer vulnerable to SQL injection
  through cell values or identifier characters.
- `Ctrl-N` inside a completion popup advances the popup instead
  of stealing focus to `:goto`. `Ctrl-P` added as the inverse.

## [1.0.0] - 2026-05-24

First public release.

- TUI database client for PostgreSQL, MySQL, SQLite, DuckDB, and
  ClickHouse (MSSQL landed in 2.0).
- Vim-style editor with completion, history, and a result pane.
- Row CRUD with a pending-changes pipeline. Mutations stage in a
  preview queue and commit in a single transaction with `Ctrl-S`.
- Built-in JSON viewer (`z` / `Z`).
- Metadata tabs on the sidebar: Records / Columns / Constraints /
  Foreign Keys / Indexes.
- Action + Keymap layer; `[keymap.<group>]` overrides in
  `config.toml`.
- `${env:VAR}` interpolation in connection params, SSH config,
  and certificate paths.
- Pre-connect shell steps with stdout capture and
  `${preconnect:NAME}` interpolation.
- `--read-only` flag and per-connection `confirm_writes`.
- MCP server (`narwhal mcp`).
- Built-in MCP tools: `list_connections`, `describe_schema`,
  `run_query`.
- Headless `narwhal exec` with CSV / TSV / JSON / Markdown /
  table / insert output formats.
- SSH tunnels.
