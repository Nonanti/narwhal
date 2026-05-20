# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
