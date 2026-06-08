# Architecture

A bird's-eye view of how the codebase is laid out and why.

## Layers

```
┌─────────────────────────────────────────────────────────────┐
│ Binary  ─  narwhaldb                                        │
│   CLI parsing, subcommand dispatch, exec / mcp / audit /    │
│   schema-diff / config / migrate-config / tui               │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────┴──────────────────────────────────┐
│ App     ─  narwhal-app                                      │
│   AppCore, run worker, dispatch, modal state machines.      │
│   Owns IO: tokio runtime, key/mouse event loop.             │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────┴──────────────────────────────────┐
│ TUI     ─  narwhal-tui                                      │
│   ratatui widgets. Pure render given a model snapshot.      │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────┴──────────────────────────────────┐
│ Domain  ─  narwhal-domain                                   │
│   Pure model state. No IO, no rendering.                    │
│   EditorBuffer, ResultView, sidebar / status / modal types. │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌────────────┬─────────────┴──────────┬────────────────────────┐
│ Commands   │  Core                  │  Subsystems            │
│ narwhal-   │  narwhal-core          │  narwhal-audit         │
│ commands   │  Connection trait,     │  narwhal-mcp           │
│  Stateless │  QueryStream, Value,   │  narwhal-pool          │
│  helpers,  │  Row, ColumnSchema,    │  narwhal-history       │
│  export,   │  TableSchema.          │  narwhal-pivot         │
│  dispatch. │                        │  narwhal-schema-diff   │
│            │                        │  narwhal-lsp           │
│            │                        │  narwhal-diagram       │
│            │                        │  narwhal-plugin        │
│            │                        │  narwhal-plugin-lua    │
│            │                        │  narwhal-plugin-wasm   │
│            │                        │  narwhal-vim           │
│            │                        │  narwhal-sql           │
└────────────┴────────────────────────┴────────────────────────┘
                           │
┌──────────────────────────┴──────────────────────────────────┐
│ Drivers ─  narwhal-drivers                                  │
│   One file per backend behind a cargo feature.              │
│   postgres / mysql / sqlite / duckdb / clickhouse / mssql.  │
└─────────────────────────────────────────────────────────────┘
```

## Dependency rules

| Crate                | May depend on                          |
|----------------------|----------------------------------------|
| `narwhal-core`       | (none in the workspace)                |
| `narwhal-domain`     | `narwhal-core`                         |
| `narwhal-config`     | `narwhal-core`, `narwhal-domain`       |
| `narwhal-sql`        | `narwhal-domain`                       |
| `narwhal-commands`   | `narwhal-core`, `narwhal-domain`, `narwhal-config`, `narwhal-sql` |
| `narwhal-drivers`    | `narwhal-core`, `narwhal-config`       |
| `narwhal-pool`       | `narwhal-core`, `narwhal-config`, `narwhal-drivers` |
| `narwhal-audit`      | `narwhal-core`, `narwhal-config`       |
| `narwhal-history`    | `narwhal-core`, `narwhal-config`       |
| `narwhal-schema-diff`| `narwhal-core`, `narwhal-domain`       |
| `narwhal-diagram`    | `narwhal-core`, `narwhal-domain`       |
| `narwhal-pivot`      | `narwhal-core`, `narwhal-domain`       |
| `narwhal-vim`        | `narwhal-domain`                       |
| `narwhal-plugin`     | `narwhal-core`                         |
| `narwhal-plugin-lua` | `narwhal-plugin`                       |
| `narwhal-plugin-wasm`| `narwhal-plugin`                       |
| `narwhal-lsp`        | `narwhal-core`                         |
| `narwhal-mcp`        | everything except `narwhal-app` / `narwhal-tui` |
| `narwhal-tui`        | `narwhal-domain`, `narwhal-commands`   |
| `narwhal-app`        | everything                             |
| `narwhaldb` (bin)    | `narwhal-app`, `narwhal-mcp`           |

`cargo deny check bans` enforces the rules.

## State ownership

| State                     | Owner                                        |
|---------------------------|----------------------------------------------|
| Open tabs, cursor, history| `narwhal-app::AppCore`                       |
| Editor buffer text        | `narwhal-domain::EditorBuffer`               |
| Result rows               | `narwhal-domain::ResultView`                 |
| Active driver connection  | `narwhal-pool::ConnectionPool`               |
| Settings                  | `narwhal-config::Settings`                   |
| Audit sink                | `narwhal-audit::AuditService`                |
| MCP server context        | `narwhal-mcp::ServerContext`                 |

## Build matrix

The binary ships every driver by default. Custom builds:

```sh
cargo build -p narwhaldb --no-default-features --features driver-sqlite
cargo build -p narwhaldb --no-default-features --features driver-postgres,driver-mysql
```

| Feature flag        | Pulls in                       |
|---------------------|--------------------------------|
| `driver-postgres`   | `tokio-postgres` + rustls      |
| `driver-mysql`      | `mysql_async` + rustls         |
| `driver-sqlite`     | `rusqlite` (bundled)           |
| `driver-duckdb`     | `duckdb` (bundled)             |
| `driver-clickhouse` | `reqwest` + rustls             |
| `driver-mssql`      | `tiberius` + rustls            |
| `all-drivers`       | All of the above               |

## See also

- [`dev/build.md`](./dev/build.md) — building from source
- [`dev/style.md`](./dev/style.md) — code style guidelines
- The crate-level rustdoc on each `narwhal-*` crate for module-level
  invariants
