# narwhal

[![CI](https://github.com/Nonanti/narwhal/actions/workflows/ci.yml/badge.svg)](https://github.com/Nonanti/narwhal/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/narwhaldb.svg)](https://crates.io/crates/narwhaldb)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](./rust-toolchain.toml)

A terminal database client for Postgres, MySQL, SQLite, DuckDB, ClickHouse,
and SQL Server — with a built-in MCP server, vim editing, and a WASM
plugin runtime.

![narwhal demo](./docs/img/demo.gif)

## Features

- **Six database engines** through one TUI: Postgres, MySQL, SQLite, DuckDB,
  ClickHouse, SQL Server.
- **MCP server** built in — `narwhal mcp` exposes `list_connections`,
  `describe_schema`, `run_query`, `explain_query`, `get_diagram`, and more
  to any Model Context Protocol client. Read-only by default, with a
  workspace ACL.
- **Three editor modes** — pick your input model in `:settings` or
  with `:mode vim|basic|emacs`. Vim ships as default with Normal /
  Insert / Visual modes, operator-pending (`dw`, `yy`, `dgg`),
  multi-cursor (`Alt-N` / `Alt-A`). Basic mode is modeless IDE-style
  (arrows, Ctrl+C/V/Z, Shift+arrow selects). Emacs mode brings the
  classic C-/M- chord set with the `C-x` prefix. All three share the
  same buffer, selection model, and undo history.
- **Full mouse support** — click to position, drag to select,
  double-click for word / triple-click for line, middle-click pastes,
  right-click opens a context menu (Cut / Copy / Paste / Select All /
  Run Selection / Find / Toggle Comment). Configurable per
  `[editor].mouse`.
- **In-app settings** — `:settings` opens a modal that drives editor
  mode, mouse mode, theme, line numbers, mode indicator, auto-indent,
  and the keybinding preset (Default / VSCode / DataGrip / IntelliJ).
  Changes save atomically to `~/.config/narwhal/config.toml` and the
  `notify`-driven watcher live-reloads any external edit.
- **Schema tooling** — ER diagrams (Focused, Impact, Mermaid / DOT
  export), `:schema-diff` between two connections with dialect-specific
  DDL emit, `:diff` for migration SQL.
- **Streaming results** — rows arrive incrementally; the result pane ticks
  live and you can cancel mid-run with `Ctrl-C`.
- **Plugin runtimes** — Lua (`~/.config/narwhal/plugins/*.lua`) and a
  WASM component-model runtime (`wasmtime` + capability sandbox).
- **Connection vault** — HashiCorp Vault and 1Password backends in
  addition to the OS keyring and `~/.pgpass`.
- **Append-only audit log** — JSONL sink for SOC 2 / ISO 27001 evidence,
  with file rotation and optional syslog.
- **Inline visualisation** — `:chart bar|line|sparkline` and
  `:pivot rows=… cols=… value=… agg=…` over any result set.
- **Export to anything** — table, CSV, JSON, TSV, Markdown, Parquet
  (with Snappy / Zstd compression).
- **SSH tunnels** — declare `ssh_host` in your connection and the local
  port forward is opened transparently.
- **Headless mode** — `narwhal exec --conn prod 'SELECT …'` for cron,
  CI, and shell pipelines.

## Install

### Cargo

```sh
cargo install narwhaldb
```

The crate name is `narwhaldb` (the bare `narwhal` slot belongs to an
unrelated 2018 Docker library); the installed binary is `narwhal`.

For users without a Rust toolchain:

```sh
cargo binstall narwhaldb
```

### Package managers

```sh
brew install Nonanti/tap/narwhal       # macOS / Linux
yay -S narwhal                          # Arch (AUR)
nix run github:Nonanti/narwhal          # Nix
```

### Pre-built binaries

Download from the [latest release](https://github.com/Nonanti/narwhal/releases):

- `x86_64-unknown-linux-gnu`
- `aarch64-apple-darwin` (Apple Silicon)

## Quick start

```sh
narwhal                        # launches the TUI
```

Then inside the TUI:

1. `:add` — connection wizard (or `:url postgres://user:pass@host/db`).
2. `:open <name>` — connect; the sidebar fills with schemas and tables.
3. **F6** — run the buffer. Results stream into the lower pane.
4. **F1** — full keymap.

### Configuration

Connections live in `~/.config/narwhal/connections.toml`:

```toml
[[connection]]
id     = "00000000-0000-0000-0000-000000000001"
name   = "prod-pg"
driver = "postgres"
color  = "red"           # status bar tint
read_only      = true    # syntactic write guard
confirm_writes = true    # type YES before any mutation

[connection.params]
host     = "db.example.com"
port     = 5432
username = "alice"
password = "${vault:secret/prod/db-password}"   # or use keyring
database = "app"
ssl_mode = "verify-full"
```

Settings live in `~/.config/narwhal/config.toml`:

```toml
theme = "dark"           # dark | light | high-contrast

[editor]
tab_width    = 4
line_numbers = true

[diagram]
icons = "ascii"          # ascii | nerdfont
```

See [`docs/`](./docs/) for vault, audit log, MCP, and plugin configuration.

## Database support

| Engine | Driver | TLS | Streaming | Cancel | DDL emit |
|---|---|---|---|---|---|
| PostgreSQL | `tokio-postgres` + rustls | ✅ verify-full | ✅ | ✅ | ✅ |
| MySQL / MariaDB | `mysql_async` | ✅ | ✅ | ✅ | ✅ |
| SQLite | `rusqlite` (bundled) | n/a | ✅ | — | ✅ |
| DuckDB | `duckdb` (bundled) | n/a | ✅ | — | ✅ |
| ClickHouse | HTTP + rustls | ✅ | ✅ | ✅ | ✅ |
| SQL Server | `tiberius` + rustls | ✅ | ✅ (buffered) | — | ✅ |

Drivers are feature-gated. The default `cargo install narwhaldb` includes
all six; build with `--no-default-features --features driver-postgres` to
slim the binary.

## MCP server

```sh
narwhal mcp                    # JSON-RPC stdio server
```

Wire into Claude Desktop:

```jsonc
{
  "mcpServers": {
    "narwhal": { "command": "narwhal", "args": ["mcp"] }
  }
}
```

A repo-local `.narwhal/workspace.toml` (committed alongside your code)
scopes what an agent can see:

```toml
allowed_connections = ["staging"]
allow_writes        = false
```

Every database call is audit-logged with `source: "mcp"`. See
[`docs/plugins/mcp-tools.md`](./docs/plugins/mcp-tools.md) for the full
tool surface.

## Headless `exec`

```sh
narwhal exec --conn prod 'SELECT count(*) FROM users'
narwhal exec -c prod -f csv 'SELECT * FROM orders' > orders.csv
narwhal exec -c prod -f json 'SELECT id, email FROM users' | jq '.[].email'

# Writes are sandboxed (BEGIN…ROLLBACK) by default. --write opts out:
narwhal exec -c prod --write 'UPDATE users SET banned = true WHERE id = 42'
```

Formats: `table`, `csv`, `json`, `tsv`, `markdown`, `parquet`.

## Plugins

### Lua (trusted)

Drop a `.lua` file in `~/.config/narwhal/plugins/`:

```lua
narwhal.register_command("rc", "row count", function(table)
  local r = narwhal.sql_run("SELECT count(*) FROM " .. table)
  return tostring(r.rows[1][1])
end)
```

Six worked examples in [`examples/plugins/`](./examples/plugins/).

### WASM (sandboxed)

Component-model plugins via `wasmtime` with a capability sandbox
(64 MiB memory, 100M fuel, 256 KiB KV store). Manifest declares
capabilities; host policy enforces them. See
[`docs/plugins/wasm.md`](./docs/plugins/wasm.md).

## Documentation

| Topic | File |
|---|---|
| Architecture & layering | [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) |
| Coding style | [`docs/STYLE.md`](./docs/STYLE.md) |
| MCP server tools | [`docs/plugins/mcp-tools.md`](./docs/plugins/mcp-tools.md) |
| Connection vault | [`docs/vault.md`](./docs/vault.md) |
| Audit log | [`docs/audit.md`](./docs/audit.md) |
| Schema diff | [`docs/schema-diff.md`](./docs/schema-diff.md) |
| LSP (embedded) | [`docs/lsp.md`](./docs/lsp.md) |
| Driver matrix | [`docs/drivers/`](./docs/drivers/) |
| WASM plugins | [`docs/plugins/wasm.md`](./docs/plugins/wasm.md) |
| Plugin security | [`docs/plugins/security.md`](./docs/plugins/security.md) |
| Release process | [`docs/RELEASING.md`](./docs/RELEASING.md) |

## Building from source

```sh
git clone https://github.com/Nonanti/narwhal.git
cd narwhal
cargo build --release          # binary at target/release/narwhal
```

Requires Rust ≥ 1.85 (edition 2024) and a C++17 toolchain for the
bundled DuckDB build. On Nix:

```sh
nix develop                    # pulls cmake, clang, libcxx, libclang
cargo build --release
```

### Tests

```sh
cargo test --workspace
```

Driver integration tests gated behind `#[ignore]` require Docker
(Postgres, MySQL, SQL Server, ClickHouse testcontainers); run with
`cargo test -- --include-ignored`.

## Upgrading from v1.x

narwhal 2.0 ships a v2 schema for `connections.toml` and `config.toml`.
On first launch against a v1 file you'll see a warning; run:

```sh
narwhal migrate-config         # writes v2 in place, keeps .v1.bak
narwhal config validate        # dry-run check
```

See [`CHANGELOG.md`](./CHANGELOG.md) for the full breaking-change list.

## Contributing

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps
```

All four must pass. New behaviour ships with a regression test. Commit
messages follow [Conventional Commits](https://www.conventionalcommits.org/)
(`feat:`, `fix:`, `refactor:`, `docs:`, `chore:`).

## License

Dual-licensed under the [MIT](./LICENSE-MIT) and
[Apache 2.0](./LICENSE-APACHE) licenses. Contributions are accepted under
the same terms.
