# Narwhal Architecture

Target architecture after the refactor. Authoritative reference for crate boundaries and dependency direction.

## Layers

```
                 narwhal (binary)
                       │
                       ▼
                  narwhal-app          ← orchestrator: wires everything
                  /    │    \
                 ▼     ▼     ▼
        narwhal-tui  narwhal-commands  narwhal-mcp
            │            │                  │
            └────────────┼──────────────────┘
                         ▼
                  narwhal-domain         ← pure state, no IO
                         │
                         ▼
                  narwhal-core           ← shared primitives
                         │
        ┌────────────────┼────────────────┐
        ▼                ▼                ▼
  narwhal-pool   narwhal-driver-registry  narwhal-sql / history / config / vim
                         │
        ┌────────────────┼────────────────┐
        ▼                ▼                ▼
   driver-postgres   driver-sqlite    driver-{mysql,duckdb,clickhouse}
```

Dependency rules:

- Arrows point **down**. No upward dependency.
- No sibling-to-sibling at the same layer except where the diagram shows.
- `narwhal-tui` knows about `narwhal-domain` (read-only). It does **not** know about `narwhal-app`, `narwhal-commands`, or any driver.
- `narwhal-app` is the only crate allowed to mutate domain state in response to user input.
- `narwhal-mcp` talks to drivers exclusively through `narwhal-driver-registry`.

## Crate responsibilities

### narwhal-core
Shared primitives: `Value`, `Row`, `ColumnSchema`, `QueryResult`, `Identifier`, `Error` types reused across the workspace. No state, no IO.

### narwhal-domain (new)
Pure model state. Owns:
- `EditorModel` — text buffer, cursor, selection, undo stack.
- `ResultModel` — rows, column metadata, sort/filter spec, viewport.
- `Tab`, `Session`, `SidebarModel`, `WizardModel`, `SnippetModel`, `HistoryModel`.

No async, no IO, no `ratatui` types. Pure data + transition methods.

### narwhal-driver-registry (new)
- `trait Driver` — connect, execute, introspect, cancel.
- `DriverKind` enum gated by features.
- `Registry` — name → factory map, populated at startup based on enabled features.
- Re-exported from `narwhal-driver-*` crates.

### narwhal-driver-{postgres,sqlite,mysql,duckdb,clickhouse}
Concrete `Driver` implementations. Each behind a feature flag in `narwhal` and `narwhal-mcp`.

### narwhal-pool
Connection pooling. Consumes `Driver`, hands out connections. No driver-specific code.

### narwhal-sql
Dialect-aware parsing, formatting, identifier quoting.

### narwhal-history
Query history persistence.

### narwhal-config
Config file loading, profile resolution.

### narwhal-vim
Vim emulation state machine. Consumes key events, returns motion intents.

### narwhal-commands (new)
Everything in today's `narwhal-app` that is "command" logic:
- Command parsing and dispatch (`:set`, `:open`, `:export`).
- Tab completion engine.
- Export pipeline (CSV, JSON, SQL dump).
- Connection wizard flow.
- Snippet management.
- DDL helpers, EXPLAIN, transactions.

Consumes `narwhal-domain` and `narwhal-driver-registry`. Returns intents/effects, does not own the runtime loop.

### narwhal-tui
Pure rendering. `ratatui` widgets. Takes `&Model` references, never mutates. Owns:
- Layout, theme, input mapping (key event → intent).
- All widgets: editor, results table, sidebar, history, snippets, wizard, help, row detail.

No business logic. A widget that needs to "do" something emits an intent.

### narwhal-app
Thin orchestrator:
- Event loop.
- Maps TUI intents + driver responses to domain transitions.
- Owns the `tokio` runtime, channels, draw scheduler.
- ≤ 2000 LOC total. ≤ 10 files.

### narwhal-mcp
MCP server. Driver-agnostic via `narwhal-driver-registry`.

### narwhal (binary)
CLI parsing, config bootstrap, terminal init, `App::run`. ≤ 400 LOC.

### narwhal-plugin / narwhal-plugin-lua
Plugin host trait + Lua implementation. Plugins consume a stable, narrow surface from `narwhal-domain` and `narwhal-commands`. Plugin-lua does **not** depend on `narwhal-core` or `narwhal-app` directly.

## State ownership

| State                         | Owner                       |
|-------------------------------|-----------------------------|
| Editor text + cursor          | `narwhal-domain::EditorModel` |
| Query results                 | `narwhal-domain::ResultModel` |
| Selected tab, focus, modals   | `narwhal-domain::Session`     |
| Vim mode                      | `narwhal-domain::Session.vim` |
| Connection pool               | `narwhal-pool` (held by `narwhal-app`) |
| Active driver registry        | `narwhal-app` (built at startup) |
| Terminal backend              | `narwhal-app::terminal` |
| Draw scheduler                | `narwhal-app::draw_scheduler` |

## Feature flags

Workspace root `narwhal` and `narwhal-mcp` expose:

```
default      = ["postgres", "sqlite"]
postgres     = ["narwhal-driver-postgres"]
sqlite       = ["narwhal-driver-sqlite"]
mysql        = ["narwhal-driver-mysql"]
duckdb       = ["narwhal-driver-duckdb"]
clickhouse   = ["narwhal-driver-clickhouse"]
all-drivers  = ["postgres", "sqlite", "mysql", "duckdb", "clickhouse"]
```

CI builds: `default`, `all-drivers`, and one minimal `--no-default-features --features sqlite` matrix entry.

## Intent / effect model

User input flows:

```
KeyEvent → narwhal-tui::input → Intent
Intent  → narwhal-app::dispatch → Domain mutation + optional Effect
Effect  → narwhal-app::executor → Driver call → DomainEvent
DomainEvent → narwhal-app::apply → Domain mutation → redraw
```

`Intent` is a closed enum in `narwhal-domain`. `Effect` is a closed enum in `narwhal-commands`. The TUI never produces an `Effect` directly.

## Non-goals (this refactor)

- No new features.
- No public CLI surface changes.
- No config schema changes.
- No protocol changes in `narwhal-mcp`.
- No driver behaviour changes.
