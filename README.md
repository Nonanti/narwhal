<div align="center">
  <img src="./docs/img/logo.png" alt="narwhal" width="160" />

  # narwhal

  **The agent-native database workbench for the terminal.**
  *A DBA toolkit, an MCP server, and a vim-mode SQL editor — in one static binary.*

  [![CI](https://github.com/Nonanti/narwhal/actions/workflows/ci.yml/badge.svg)](https://github.com/Nonanti/narwhal/actions/workflows/ci.yml)
  [![Crates.io](https://img.shields.io/crates/v/narwhaldb.svg)](https://crates.io/crates/narwhaldb)
  [![Downloads](https://img.shields.io/crates/d/narwhaldb.svg)](https://crates.io/crates/narwhaldb)
  [![MSRV](https://img.shields.io/badge/rustc-1.85+-blue.svg)](https://blog.rust-lang.org/)
  [![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

  [Install](#install) · [Quick start](#quick-start) · [Why narwhal](#why-narwhal) · [Docs](#documentation)
</div>

---

![narwhal demo](./docs/img/demo.gif)

## What is narwhal?

narwhal is a terminal database client built around two things every
other client misses:

1. **DBA-grade tooling, not a toy REPL.** Schema diff with dialect-aware
   DDL emit, append-only audit log, capability-gated plugin sandbox,
   streaming cancellable queries, ER diagrams, SSH tunnels, secret
   vault — the operational surface a real database day needs.
2. **First-class agent integration.** A built-in MCP server exposes
   your connections, schema, and query execution to Claude, Cursor,
   Zed, and any MCP-aware client. No proxy, no adapter — `narwhal mcp`
   and you're wired.

Six engines (Postgres, MySQL, SQLite, DuckDB, ClickHouse, SQL Server),
three editor modes (vim / basic / emacs), one static binary.

## Install

```sh
curl -fsSL https://github.com/Nonanti/narwhal/releases/latest/download/install.sh | sh
```

Other channels — `cargo install narwhaldb`, Homebrew, AUR, Nix flake,
prebuilt tarballs — are listed in [`docs/install.md`](./docs/install.md).

## Quick start

```sh
narwhal                                    # launches the TUI
```

Inside:

1. `:add` — connection wizard, or `:url postgres://user:pass@host/db`
2. `:open <name>` — connect; the sidebar fills with schemas and tables
3. **F6** — run the buffer; results stream into the lower pane
4. **F1** — full keymap

Headless:

```sh
narwhal exec --conn prod 'SELECT count(*) FROM orders' --format json
```

## Agent-native: wire it to your editor

narwhal embeds an [MCP](https://modelcontextprotocol.io/) server over
stdio. Run it once and your AI assistant can browse schema, sample
data, and run queries against your saved connections — with the same
read-only / per-connection guards the TUI uses.

```sh
narwhal mcp --read-only
```

Claude Desktop (`~/.config/Claude/claude_desktop_config.json`):

```jsonc
{
  "mcpServers": {
    "narwhal": {
      "command": "narwhal",
      "args": ["mcp", "--read-only"]
    }
  }
}
```

Cursor, Zed, Continue, Aider, and custom clients work the same way.
Full setup: [`docs/mcp.md`](./docs/mcp.md).

## Database support

| Engine | Driver | TLS | Streaming | Cancel | DDL emit |
|---|---|---|---|---|---|
| PostgreSQL | `tokio-postgres` + rustls | verify-full | ✅ | ✅ | ✅ |
| MySQL / MariaDB | `mysql_async` | ✅ | ✅ | ✅ | ✅ |
| SQLite | `rusqlite` (bundled) | n/a | ✅ | — | ✅ |
| DuckDB | `duckdb` (bundled) | n/a | ✅ | — | ✅ |
| ClickHouse | HTTP + rustls | ✅ | ✅ | ✅ | ✅ |
| SQL Server | `tiberius` + rustls | ✅ | buffered | — | ✅ |

## Highlights

- **Six engines, one TUI.** Switch with `:open`, every driver speaks the
  same `Connection` trait — same streaming, same cancellation, same
  schema introspection.
- **Three editor modes.** vim (with motions, text objects, registers,
  marks, macros), basic (modeless), emacs — sharing one buffer.
- **Full mouse.** Click, drag-select, double / triple-click word and
  line, middle-click paste, right-click context menu.
- **Schema tooling.** ER diagrams in the terminal, `:schema-diff`
  between two connections with dialect-aware DDL emit (Postgres ↔
  MySQL ↔ MSSQL ↔ generic).
- **Plugins.** Lua (trusted, fast) and WASM (sandboxed, capability-gated
  filesystem / network / clipboard) — the same surface used by the
  built-ins.
- **Streaming, cancellable.** Rows arrive incrementally; `Ctrl-C`
  cancels mid-run on every backend that supports it.
- **Secrets.** Vault, 1Password, OS keyring, `~/.pgpass` — no
  plaintext-on-disk fallback.
- **Audit log.** Append-only JSONL with redaction rules, suitable as
  compliance evidence.
- **Inline output.** Charts, pivot tables, JSON viewer, row detail
  pane, exports to CSV / JSON / Markdown / Parquet.
- **SSH tunnels.** Single config block, no `~/.ssh/config` plumbing
  required.

## Documentation

| Topic | File |
|---|---|
| Install (all methods) | [`docs/install.md`](./docs/install.md) |
| Configuration | [`docs/configuration.md`](./docs/configuration.md) |
| Editor modes | [`docs/editor-modes.md`](./docs/editor-modes.md) |
| Mouse | [`docs/mouse.md`](./docs/mouse.md) |
| MCP server | [`docs/mcp.md`](./docs/mcp.md) |
| Headless `exec` | [`docs/headless.md`](./docs/headless.md) |
| Plugins (Lua + WASM) | [`docs/plugins.md`](./docs/plugins.md) |
| Connection vault | [`docs/vault.md`](./docs/vault.md) |
| Audit log | [`docs/audit.md`](./docs/audit.md) |
| Schema diff | [`docs/schema-diff.md`](./docs/schema-diff.md) |
| Architecture | [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) |
| Building from source | [`docs/dev/build.md`](./docs/dev/build.md) |
| Upgrading | [`docs/upgrading.md`](./docs/upgrading.md) |

## Contributing

Issues and PRs welcome. See [`CONTRIBUTING.md`](./CONTRIBUTING.md).
Commit messages follow [Conventional
Commits](https://www.conventionalcommits.org/); the four CI checks
(`fmt`, `clippy -D warnings`, `doc -D warnings`, `test`) must pass.

## License

Dual-licensed under [MIT](./LICENSE-MIT) and
[Apache 2.0](./LICENSE-APACHE), at your option.
