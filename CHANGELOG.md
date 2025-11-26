# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
