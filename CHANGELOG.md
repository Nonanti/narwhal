# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
