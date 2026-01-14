# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

_Nothing yet — the next entry will land here when the first post-v1.1
change is merged. See `[1.1.0]` below for the most recent release._

## [1.1.0] — 2026-05-22

First post-v1.0 release. Headline feature: narwhal now ships a built-in
Model Context Protocol server so AI agents can use the same connection
config the TUI does. No breaking changes to the public Rust API or to
the on-disk configuration formats.

### Added

- **MCP server** (`narwhal-mcp` crate + `narwhal mcp` subcommand).
  Exposes narwhal's configured connections to AI agents (Claude Desktop,
  Cursor, Continue, Aider, …) over the canonical Model Context Protocol
  stdio transport (JSON-RPC 2.0, protocol version `2024-11-05`). v0 ships
  five tools:
  - `list_connections` — read-only catalogue of the connections defined
    in `~/.config/narwhal/connections.toml`. No IO, no credentials
    loaded. Honours the workspace ACL when one is attached.
  - `describe_schema` — opens a short-lived connection and returns the
    schema/table/view tree.
  - `describe_table` — full `TableSchema` for one table (columns,
    indexes, foreign keys, unique constraints) plus engine-native DDL
    when the driver supports `fetch_ddl`.
  - `run_query` — executes a single statement. Defaults to
    **read-only** with three layers of defence: a syntactic guard
    (statement must start with `SELECT/WITH/SHOW/EXPLAIN/DESCRIBE/
    PRAGMA/VALUES/TABLE`), a `BEGIN ... ROLLBACK` sandwich, and a row
    limit (default 1 000, max 10 000). `read_only=false` disables all
    three but is itself rejected when the active workspace forbids
    writes. Every call is audit-logged.
  - `explain_query` — driver-native EXPLAIN with the right prefix per
    dialect (`EXPLAIN (VERBOSE)` on Postgres, `EXPLAIN QUERY PLAN` on
    SQLite, `EXPLAIN PLAN` on ClickHouse, …). Optional `analyze=true`
    runs the statement to gather real cardinalities on engines that
    support it (PG / MySQL / DuckDB).

  Credential resolution mirrors the TUI: keyring first,
  `~/.pgpass`/env fallback second.

  Wire-up for Claude Desktop:

  ```jsonc
  // ~/.config/Claude/claude_desktop_config.json
  {
    "mcpServers": {
      "narwhal": { "command": "narwhal", "args": ["mcp"] }
    }
  }
  ```

  Logs go to stderr in MCP mode (stdout is the JSON-RPC transport).

- **MCP audit log.** `narwhal-history::HistoryEntry` gained a
  `source: Option<String>` field (backward-compatible via
  `#[serde(default)]`). Every MCP tool that touches a database appends
  an entry tagged `"mcp"` to the existing `history.jsonl` so operators
  can `jq 'select(.source == "mcp")'` to isolate agent-issued
  traffic. Write calls (`read_only=false`) prepend a
  `-- mcp: read_only=false` marker to the recorded SQL.

- **Workspace file** — `.narwhal/workspace.toml`. A repo-local file
  (discovered by walking up from `pwd`, same idiom as `.git`) declares
  which subset of `connections.toml` the MCP server may expose and
  whether writes are permitted. The TUI ignores the file for now
  — v1.1 will wire it up across the application. Schema:

  ```toml
  allowed_connections = ["staging", "test"]   # empty = all
  allow_writes = false                         # default true
  ```

  `deny_unknown_fields` is on so a typo like `allow_write` fails
  loudly instead of silently being permissive.

### Fixed

- **ClickHouse driver: drop transitive openssl dependency.**
  `narwhal-driver-clickhouse` declared `reqwest` with the `rustls-tls`
  feature but kept `default-features = true`, so the `default-tls` chain
  (native-tls → openssl-sys) was still active. The workspace already
  avoids OpenSSL elsewhere (the `keyring` crate uses `crypto-rust`).
  Setting `default-features = false` and re-listing the actually-needed
  default features (`charset`, `http2`, `system-proxy`) removes
  `openssl-sys` from the whole workspace dependency graph, drops the
  `libssl-dev` requirement for Linux builds and shrinks the audit
  surface.

## [1.0.0] — 2026-05-22

First tagged release. The lines below are grouped by area; the
**Phase 1 → 3** suffixes refer to the staged hardening sweep that
landed immediately before tagging — they are noted for traceability
but do not affect the public API.

### Highlights

- Five-driver parity (Postgres, MySQL, SQLite, DuckDB, ClickHouse)
  with streaming, query cancel, prepared-statement caches where
  applicable, and a unified TLS surface.
- Vim editing model with auto-pair, schema-aware completion (alias
  resolved, schema-qualified, built-in functions), result-pane sort
  and filter, and a `:` command palette.
- Lua plugin runtime with command, transform and `sql_run` hooks;
  ships with six sample scripts in `examples/plugins/`.
- Connection UX: `:url`, `:add`, `:edit`, `:test`, last-used
  ordering, SSH tunnels, `~/.pgpass` + env-var password fallback,
  OS keyring credential storage.
- Performance hardening (Phase 2): `~6×` faster JSON column sort,
  `~6×` faster vim word-motion at 5 000 lines, structural JSON
  compare without `to_string()` allocation, zero-clone history
  append for the no-secret path.
- Correctness hardening (Phase 1): 14 bugs fixed across SSH probe,
  password redaction, transaction unwinding, percent-decoding,
  describe-table observability, and the `:export` arg parser. Five
  new regression tests guard the affected paths.
- Settings wiring (Phase 3): a one-line `config.toml` with
  `theme = "light"` etc. is now honoured at start-up; malformed
  files emit a load-time warning.
- `cargo-deny` advisory and licence gate added to CI; `deny.toml`
  at the repo root.

### Added

- **Sidebar viewport scrolling** (L24). The connection / schema browser
  now clamps a `scroll_offset` against the visible viewport so long
  catalogues (> N items) stay navigable. `PageDown`/`PageUp` (or
  `Ctrl-d`/`Ctrl-u`) jump ten rows; `Home`/`End` snap to the
  endpoints; mouse wheel over the sidebar pans the viewport by 3 rows
  without moving the selection. New
  `SidebarView::{visible_rows, clamp_scroll}` helpers live in
  `narwhal-tui`.
- **MySQL `KILL QUERY` cancel** (L31). `MysqlConnection` now captures
  `CONNECTION_ID()` at connect time and exposes a `cancel_handle()`
  that opens a second connection to issue `KILL QUERY <thread_id>`.
  Brings F4 cancel parity with the PostgreSQL and ClickHouse drivers.

### Breaking

- **Public enums marked `#[non_exhaustive]`** (M14). Every public
  enum in `narwhal-core`, `narwhal-sql`, `narwhal-history`,
  `narwhal-config`, `narwhal-pool`, `narwhal-plugin`, `narwhal-vim`,
  `narwhal-tui`, and `narwhal-app` now carries `#[non_exhaustive]`
  so future variant additions are not SemVer-breaking. Downstream
  callers must add a wildcard arm to any `match` that consumes one
  of these enums from outside the defining crate.
- **`ResultView.state` is now `pub(crate)`** (M22). Use
  `ResultView::selected`, `select`, `scroll_offset`,
  `set_scroll_offset` instead of touching `TableState` directly
  — protects callers from a future ratatui major upgrade.
- **`Tab` storage fields are now `pub(crate)`** (L23). Downstream
  callers use the new read-only / mutable getter pairs: `name()`,
  `editor()`/`editor_mut()`, `results()`/`results_mut()`,
  `editor_search()`, `page_size()`, `completion()`. Lets us evolve
  the per-tab storage shape without SemVer breakage.

### Refactor

- **`narwhal-tui` no longer depends on `narwhal-sql`** (H18).
  `EditorBuffer` is strictly text+cursor; `statement_at_cursor` /
  `all_statements` moved to `narwhal_app::editor`.
- **`crates/narwhal-app/src/core.rs` (4858 lines) split into a
  `core/` module tree** (L21): mod.rs + 12 submodules
  (`text_utils`, `render_helpers`, `plugin_executor`,
  `transactions`, `plugins`, `run_loop`, `tabs`, `sessions`,
  `results_actions`, `editor_handlers`, `dump_export`, `modals`).
  mod.rs is now ~1340 lines.
- **TUI sizing constants centralised** (M23) in a new
  `narwhal_tui::constants` module.

### Performance

- **ClickHouse `http_query` returns `bytes::Bytes`** (L28) instead of
  copying the response body into an owned `Vec<u8>`. Halves the peak
  RSS on large `SELECT` results that take the non-streaming path.
- **UI no longer blocks on background metadata** (H11). New
  `narwhal-app::meta` module ships a `MetaRequest`/`MetaUpdate`
  channel; `dump_schema all`, `refresh_schemas`, and the Ctrl+R
  history modal now dispatch via the channel instead of
  `tokio::task::block_in_place` + `Handle::current().block_on(...)`,
  so F4 (cancel) and key events stay responsive while the worker
  task is busy.
- **Schema refresh is no longer N+1** (H12). `Connection` gains a
  `list_all_tables` trait method with a default fallback to the
  legacy `list_schemas` + per-schema `list_tables` loop. Postgres,
  MySQL, SQLite, DuckDB, and ClickHouse override it with a single
  catalogue query (`information_schema.tables`, `sqlite_master`,
  `duckdb_tables UNION duckdb_views`, `system.tables`), eliminating
  one round trip per schema during sidebar refresh.
- **PostgreSQL prepared-statement cache** (M9). Schema/admin
  queries now flow through a per-connection 64-entry LRU cache,
  cutting `describe_table` from 8 round trips to 2 on warm cache.
- **Lua timeout hook no longer locks a Mutex per line** (H15). The
  budget structure became `Arc<InvocationTimeout>` with an
  `AtomicBool` flag, eliminating contention and Mutex-poisoning
  risk in tight transform loops.
- **History `Journal::recent` reads in reverse and off-thread**
  (M13). The journal is now scanned from the tail via `rev_lines`
  inside `spawn_blocking`, stopping as soon as N entries are
  gathered; parse errors are logged via `tracing::warn` instead of
  being silently swallowed.
- **`Value::Display` writes straight to the formatter** (L1). The
  integer/float/date paths no longer materialise an intermediate
  `String` before printing.
- **`Splitter::find_dollar_close` uses `memchr::memmem`** (L3),
  replacing the byte-by-byte scan inside long PL/pgSQL bodies.

### Cleanup

- **Editor gutter width is now dynamic** (L36) so buffers with
  > 999 lines render correctly.
- **`wrap_text` uses unicode-segmentation graphemes** (L17) for
  long-word breaks instead of byte-chunking through multi-byte
  UTF-8.
- **`EditorBuffer::move_word_forward` skips newlines** (L16) so
  `w` lands on the next word across line breaks.
- **Vim `command_buffer` capped at 4 KiB** (L14).
- **`Pane::cycle_back` added** (L27) for reverse focus rotation
  via Shift+Ctrl+W.
- **`Hash` derives added** on narwhal-vim `Mode`, `KeyCode`,
  `KeyMod`, `Operator` (L13).
- **`centred_rect` deduplicated** (L25) into
  `widgets::centred_rect` shared by every modal renderer.
- **`narwhal-tui::lib.rs` re-exports via glob** (L26) instead of
  the duplicated explicit list.
- **First tab named `untitled-1`** (L33) to match `untitled-N`.
- **Dropped unused `tracing` dep** from `narwhal-tui` (L35).
- **`ConfigPaths::ensure` returns path-aware errors** (L37).
- **History journal caps SQL at 64 KiB** (L38) with a
  `… (truncated N bytes)` suffix.
- **`narwhal::main` drops the tracing guard before `exit(1)`**
  (L40) so the final log event reaches disk.
- **`narwhal::main` logs settings/connections load failures** (L20).
- **`expect("plugin_state poisoned")` replaced by
  `unwrap_or_else(|e| e.into_inner())`** (L22). Poisoned mutex no
  longer crashes the app.
- **`ClickHouse` `active_queries` uses `parking_lot::Mutex`** (L2)
  instead of `tokio::sync::Mutex`.
- **`format_count` / `format_elapsed` boundary rounding fixed**
  (L18, L19): `999_999` → `1.0M`; `59_999ms` → `01:00`.
- **`Pool::new` asserts `max_size > 0`** (L6).
- **`parse_url` accepts IPv6 hosts in `[brackets]`** (L4);
  empty query keys error via `UrlError::EmptyQueryKey` (L7).
- **`validate_connections` rejects duplicate UUIDs** (L5).
- **PG `Param` uses `try_from`** for INT2/INT4/OID binds (L8)
  instead of silently truncating `as` casts.
- **PG DDL emitter rejects generated-stored columns with no
  expression** (L9).
- **`find_all` `.max(1)` dead guard removed** (L32). DuckDB stream
  empty-branch uses `drop(tx)` (L10). Editor `pos = end.max(...)`
  simplified to `pos = end` (L15).

### Fixed

- **PostgreSQL `extract_csv` handles commas in identifiers** (M8).
  `string_agg` now joins with the ASCII unit separator (U+001F) and
  the parser splits on the same byte, so a column named `"a,b"` no
  longer corrupts schema introspection.
- **PostgreSQL cancel/cursor handle uses `Statement` from cache, not
  re-prepares each call** — see M9 above.
- **ClickHouse Float NaN / ±Inf binds as `nan()` / `inf()` /
  `-inf()`** (M5) instead of the SQL-invalid literals `NaN`, `inf`,
  and `-inf`.
- **ClickHouse `cancel()` is idempotent** (M6). The query-id set is
  no longer drained on the first call; every subsequent Ctrl-C
  re-issues `KILL QUERY` for the still-running query ids.
- **ClickHouse `stream` no longer leaks `query_id` on the error
  path** (M7). A `QueryGuard` removes the id from the active set on
  drop, covering panics, cancellations, and stream-task aborts.
- **DuckDB renders Date32 / Time64 / Timestamp / Interval as proper
  chrono types** (M12), not as `"date(19876)"` strings, so CSV
  export, clipboard, and column sort behave correctly.
- **`describe_table` reports `TableKind::View` on SQLite, DuckDB,
  and ClickHouse** (M11), matching the MySQL fix in Wave 3 and the
  Postgres baseline. Views, materialised views, and system views
  now show the correct sidebar glyph.
- **`narwhal-pool` removes `unwrap`/`expect`** (H19). The poison-able
  `std::sync::Mutex` was replaced with `parking_lot::Mutex` and the
  `PooledConnection` invariants are now encoded via
  `ManuallyDrop`/`Option` so no defensive `expect` remains.
- **Editor search highlight + history modal honour char boundaries
  and display width** (H16). Multi-byte and East-Asian wide
  characters no longer panic the highlighter or overflow the
  modal's column budget.
- **Completion popup geometry agrees with hit-testing** (H17).
  `render_completion_popup` returns the actual `Rect` chosen at
  render time so the layout regions used by mouse dispatch always
  match what the user sees.
- **Vim operators (`d`, `y`, `c`) work** (M16). The Vim state
  machine gained `Mode::OperatorPending(Operator)`; `dw`, `yy`,
  `c$`, etc. now compose correctly with motions and counts.
  `pending_count` is saturating-adds protected (M17).
- **Plugin command timeout reports the resolved plugin name** (H20).
  The plugin handle is captured before dispatch so the error
  message attributes the timeout to the right plugin even when two
  plugins register the same head; the message now references
  `narwhal.execution_timeout_secs` so the user knows what to tune.
- **Lua `_timeout_budget` is no longer script-accessible** (M18).
  The budget lives in the Lua registry under an opaque key instead
  of a global, so plugins cannot disable their own timeout. Plugin
  names are derived from the file stem (M19) so a restart of
  Narwhal does not change `:foo` into `:plugin_a4d2`.
- **Mouse table preview keeps the cell-edit path** (M15). Clicking
  a sidebar table now routes through `run_preview`, so the preview
  result behaves identically to the keyboard shortcut.
- **TUI sanitises BIDI and control characters in grid display**
  (M20). U+202E, U+200E, soft-hyphens, etc. render as a visible
  middle-dot so a malicious value cannot reorder the cell.
- **Status bar width uses Unicode display width** (M21), not
  `chars().count()`, so wide-character session names no longer
  overrun the bar.

### Changed

- **MySQL parameterless queries now use the binary prepared-statement
  protocol** (H4) instead of falling through to the text protocol
  whenever `params.is_empty()`. A small whitelist keeps transaction
  control, session state (USE/SET), catalogue introspection
  (SHOW/DESCRIBE/EXPLAIN), lock management, FLUSH/RESET/KILL/PURGE,
  LOAD, and HANDLER on the text protocol where MySQL refuses to
  prepare them. Everything else goes through `exec_iter` so column
  type information survives end-to-end (`SELECT 1` is now
  `Value::Int(1)`, not `Value::String("1")`).
- **`Capabilities` gains a `streaming` flag** (H5). Postgres, SQLite,
  DuckDB, and ClickHouse advertise `true` (genuine row-by-row
  streaming); MySQL declares `false` because its `stream()` still
  materialises the full result through `BufferedRowStream`. The UI
  can now warn before opening open-ended streams against MySQL.
- **SQL splitter understands MySQL backslash escapes and PostgreSQL
  E-strings** (H10). `State::StringLiteral` carries a
  `backslash_escape` flag: MySQL turns it on for every single-quoted
  literal, PostgreSQL only when the token immediately preceding the
  quote is an `E`/`e` at a token boundary. Standard SQL `''` is still
  recognised by every dialect.
- **MySQL `describe_table` reports the correct `TableKind`** (L30).
  Previously hard-coded `TableKind::Table`; now queries
  `information_schema.tables.TABLE_TYPE` through a shared
  `map_table_kind` helper, matching `list_tables`. Views, system
  views, and system tables are surfaced in their proper categories.
- **MySQL `describe_table` surfaces single-column UNIQUE constraints**
  (M10). Dropped the `columns.len() > 1` arity guard; PRIMARY KEY is
  still excluded because it is already exposed via
  `Column.primary_key`.
- **MySQL BLOB values stay as `Value::Bytes`** (L29). `value_from_my`
  takes a `ColumnType` and short-circuits the UTF-8 decode for
  `MYSQL_TYPE_*BLOB` and `GEOMETRY` columns, even when the payload
  happens to be valid UTF-8 (small ASCII blobs).

### Security

- **Postgres `Prefer`/`Require` no longer skip certificate verification**
  (H1, M1, M2). The default `Prefer` and `Require` modes now use the
  system root store with chain verification; `verify-ca` uses a custom
  verifier that skips only the hostname check. **Breaking:** existing
  self-signed servers reached via `Prefer`/`Require` will now be
  rejected — see README “TLS defaults changed” for migration.
- **Postgres connection-string injection closed** (H2). The driver no
  longer concatenates user-supplied values into a libpq string; it
  uses `tokio_postgres::Config` builder with a whitelisted `options`
  set.
- **Postgres cancel handle now uses the same TLS connector** (H3) as
  the live connection, so cancellation works on TLS-only servers.
- **History JSONL redacts secrets and is created mode 0600** (H7).
  `PASSWORD '...'`, `IDENTIFIED BY '...'`, `CREDENTIALS '...'`, and
  `SET PASSWORD = '...'` are masked before the line is written. File
  mode is enforced on Unix; pre-existing history files are left
  untouched.
- **Keyring access moved off the tokio runtime thread** (H8).
  `CredentialStore` is now an async trait backed by
  `spawn_blocking`; a locked or unresponsive Secret Service no longer
  stalls UI tasks.
- **URL query parser routes `sslmode`/`sslrootcert`/`sslcert`/`sslkey`
  into struct fields** (H9) instead of dropping them into the generic
  `options` map. Unknown `sslmode` values now produce a typed error.
- **Wizard passwords are kept in `SecretString` and zeroized on
  drop** (H13). `secrecy` / `zeroize` added as workspace deps;
  `commit_wizard` exposes the secret exactly once when handing it to
  the keyring.
- **Config rejects `ssl_mode = disable` with `ssl_root_cert`/`ssl_cert`/`ssl_key` set**
  (M3). Misconfiguration that previously degraded silently to plain
  TCP now surfaces a validation error.
- **ClickHouse `escape_sql_string` escapes backslashes** (M4), closing
  a literal-injection edge case where `\'` could prematurely close a
  string.

### Fixed

- **MySQL Date/Time bind round-trip** (C1): years outside `u16`,
  dropped microseconds, and `Value::Timestamp` rejected as RFC3339 are
  all fixed. The bind path now uses `chrono::Datelike`/`Timelike`
  directly and returns a typed error on out-of-range years instead of
  silently storing `0000-00-00`. (Also fixes H6.)
- **ClickHouse parameter substitution UTF-8** (C2): non-ASCII
  identifiers (`"kullanıcılar"`) and string literals (`'çöğşüı'`,
  `'🦀 narwhal'`) survive parameter substitution intact. Also closes a
  dollar-misfire where `'$1.99'` literals tripped the `$N` placeholder
  path.
- **DuckDB `RETURNING` detection no longer panics** on multibyte SQL
  (C3). The 9-byte window comparison switched from `&str` slicing to
  byte-slice `eq_ignore_ascii_case`.
- **Editor cursor on Turkish / CJK / emoji input** (C4). `cursor_x`
  now reflects East-Asian display width and `EditorBuffer::set_cursor`
  snaps back to a UTF-8 char boundary before storing the column.
- **Schema refresh after DDL targets the originating session** (C5).
  `RunUpdate::SchemaRefresh` carries a `session_id`; the handler drops
  the notification if the user has switched sessions during the
  200 ms debounce window.
- **Streaming render throttle re-engaged** (C6). `App::run` now gates
  redraws through a `DrawScheduler` that coalesces `RowsAppended`
  events into one draw per 100 ms window, with a deadline tick to
  flush the trailing batch. Force events (key, mouse, non-stream
  updates) bypass the throttle.

## [1.0.0] — 2026-05-20

### Added

- **DX polish** (Plan 04): more sample plugins (`:help <command>`) and
  built-in help improvements.
- **ClickHouse correctness** (Plan 05): byte-accurate TSV decoding,
  stream cleanup, mid-row truncation handling, and body decode errors.
- **DataGrip parity** (Plan 06): status bar split (mode / connection /
  transaction / message), mouse support across panes, context-aware
  completion for FROM/JOIN/UPDATE and dotted access, column sort and
  substring filter, Ctrl+R history modal, vim-style `/` search and
  `:s` substitute, auto-pair brackets/quotes, help panel cheatsheet,
  prompt tab-completion for `:open`/`:help`/`:export`.
- **Result export** (Plan 07-01): `:export csv|json|insert <path>`
  writes the visible result set to disk.
- **Row detail modal** (Plan 07-02): expand wide rows in a full-screen
  overlay.
- **Multi-statement tabs** (Plan 07-03): tab strip for result bundles
  produced by multi-statement queries.
- **Streaming row counter** (Plan 07-04): live row count for streaming
  queries.
- **Schema refresh** (Plan 07-05): `:refresh` command + auto schema
  reload on DDL.
- **DDL generation** (Plan 07-06): `d` on a sidebar table fetches and
  injects DDL.
- **Saved queries** (Plan 07-07): snippets library for frequently-used
  queries.
- **TLS options** (Plan 07-08): TLS / SSL configuration across the
  network drivers.
- **Driver byte tests** (Plan 07-09): byte-accurate row invariants for
  every driver.
- **Plugin timeout** (Plan 07-10): Lua execution timeout via mlua hook.
- **README** (Plan 07-11): install instructions, feature overview,
  screenshots.
- **Distribution** (Plan 07-12): crates.io metadata, AUR PKGBUILD
  template, Homebrew formula template, release procedure doc.

[1.0.0]: https://github.com/berkant/narwhal/releases/tag/v1.0.0
