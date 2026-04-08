# narwhal

[![CI](https://github.com/Nonanti/narwhal/actions/workflows/ci.yml/badge.svg)](https://github.com/Nonanti/narwhal/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/narwhaldb.svg)](https://crates.io/crates/narwhaldb)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

A terminal database client for **Postgres, MySQL, SQLite, DuckDB,
ClickHouse, and SQL Server** — with a built-in MCP server, three
editor modes, and a WASM plugin runtime.

![narwhal demo](./docs/img/demo.gif)

## Install

```sh
curl -fsSL https://github.com/Nonanti/narwhal/releases/latest/download/install.sh | sh
```

Other methods (cargo, brew, AUR, Nix, prebuilt binaries):
[`docs/install.md`](./docs/install.md).

## Quick start

```sh
narwhal                        # launches the TUI
```

Inside the TUI:

1. `:add` — connection wizard (or `:url postgres://user:pass@host/db`)
2. `:open <name>` — connect; the sidebar fills with schemas and tables
3. **F6** — run the buffer; results stream into the lower pane
4. **F1** — full keymap

## Features

- **Six engines, one TUI** — Postgres, MySQL, SQLite, DuckDB, ClickHouse, SQL Server
- **Three editor modes** — vim (default), basic (modeless), emacs — all sharing one buffer
- **Full mouse support** — click, drag-select, double/triple-click, middle-paste, right-click menu
- **MCP server** — `narwhal mcp` exposes connections, schema, query, diagrams to any agent
- **Plugins** — Lua (trusted) and WASM (sandboxed, capability-gated)
- **Schema tooling** — ER diagrams, `:schema-diff`, dialect-aware DDL emit
- **Streaming results** — rows arrive incrementally; cancel mid-run with `Ctrl-C`
- **Secret backends** — Vault, 1Password, OS keyring, `~/.pgpass`
- **Audit log** — append-only JSONL for SOC 2 / ISO 27001
- **Headless `exec`** — for cron, CI, shell pipelines
- **SSH tunnels, inline charts, pivot tables, export to anything** (CSV/JSON/Markdown/Parquet)

## Database support

| Engine | Driver | TLS | Streaming | Cancel | DDL emit |
|---|---|---|---|---|---|
| PostgreSQL | `tokio-postgres` + rustls | ✅ verify-full | ✅ | ✅ | ✅ |
| MySQL / MariaDB | `mysql_async` | ✅ | ✅ | ✅ | ✅ |
| SQLite | `rusqlite` (bundled) | n/a | ✅ | — | ✅ |
| DuckDB | `duckdb` (bundled) | n/a | ✅ | — | ✅ |
| ClickHouse | HTTP + rustls | ✅ | ✅ | ✅ | ✅ |
| SQL Server | `tiberius` + rustls | ✅ | ✅ (buffered) | — | ✅ |

## Documentation

| Topic | File |
|---|---|
| Install (all methods)   | [`docs/install.md`](./docs/install.md) |
| Configuration           | [`docs/configuration.md`](./docs/configuration.md) |
| MCP server              | [`docs/mcp.md`](./docs/mcp.md) |
| Headless `exec`         | [`docs/headless.md`](./docs/headless.md) |
| Editor modes            | [`docs/editor-modes.md`](./docs/editor-modes.md) |
| Mouse                   | [`docs/mouse.md`](./docs/mouse.md) |
| Plugins (Lua + WASM)    | [`docs/plugins.md`](./docs/plugins.md) |
| Connection vault        | [`docs/vault.md`](./docs/vault.md) |
| Audit log               | [`docs/audit.md`](./docs/audit.md) |
| Schema diff             | [`docs/schema-diff.md`](./docs/schema-diff.md) |
| Architecture            | [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) |
| Building from source    | [`docs/dev/build.md`](./docs/dev/build.md) |
| Upgrading               | [`docs/upgrading.md`](./docs/upgrading.md) |

## Contributing

See [`CONTRIBUTING.md`](./CONTRIBUTING.md). Commit messages follow
[Conventional Commits](https://www.conventionalcommits.org/) and the
four CI checks (`fmt`, `clippy -D warnings`, `doc -D warnings`,
`test`) must pass.

## License

Dual-licensed under [MIT](./LICENSE-MIT) and
[Apache 2.0](./LICENSE-APACHE).
