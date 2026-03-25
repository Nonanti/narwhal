# narwhal

[![CI](https://github.com/Nonanti/narwhal/actions/workflows/ci.yml/badge.svg)](https://github.com/Nonanti/narwhal/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#licence)
[![Version](https://img.shields.io/badge/version-1.1.0-brightgreen)](./CHANGELOG.md)
[![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](./rust-toolchain.toml)

> Multi-driver TUI database client with a built-in MCP server.
> Postgres / MySQL / SQLite / DuckDB / ClickHouse / SQL Server, vim editing, Lua plugins.

![narwhal demo](./docs/img/demo.gif)

## Why narwhal

- **Built-in MCP server.** Run `narwhal mcp` and any
  [Model Context Protocol](https://modelcontextprotocol.io/) client (Claude
  Desktop, Cursor, your own agent) gets `list_connections`,
  `describe_schema`, `describe_table`, `run_query`, `explain_query`,
  `get_diagram` over stdio. Read-only by default, with a three-layer
  SQL guard and a workspace ACL (`.narwhal/workspace.toml`) so an
  agent can only see the connections you explicitly listed.
- **ER diagrams without leaving the terminal.** `:diagram users`
  opens a Focused modal with the table, its columns, and every
  in/out-bound foreign-key neighbour; `:diagram impact users` shows
  the reverse-FK closure ("what breaks if I drop this?");
  `:diagram export mermaid` ships Mermaid source straight to your
  clipboard for mermaid.live, a PR description, or Notion.
- **One TUI, six databases.** Postgres, MySQL, SQLite, DuckDB,
  ClickHouse, Microsoft SQL Server. No driver-juggling, no context-switching between `psql`,
  `mysql`, and DataGrip.
- **Vim editing + auto-pair + completion.** Modal input (Normal,
  Insert, Visual), schema-aware tab-completion, alias-resolved column
  hints, a proper `:` command palette.
- **Lua plugin runtime.** The bits that should be yours, stay yours.
  Write a `.lua` file, drop it in `~/.config/narwhal/plugins/`, and it
  is live.
- **SSH tunnels, `~/.pgpass`, OS keyring.** The auth ergonomics you
  already configured for `psql` work here too. Set
  `ssh_host=jump.example.com` and the connect path forwards a loopback
  port for you.

## Install

### Cargo (any platform with Rust)

```sh
cargo install narwhaldb
```

> The crates.io name is `narwhaldb` (the bare `narwhal` slot is held
> by an unrelated 2018 docker library); the installed binary is still
> just `narwhal`.

For users without a Rust toolchain, `cargo-binstall` fetches the
prebuilt binary instead of compiling:

```sh
cargo binstall narwhaldb
```

### Homebrew (macOS, Linuxbrew)

```sh
brew tap Nonanti/tap
brew install narwhal
```

### Arch Linux (AUR)

```sh
yay -S narwhal      # or: paru -S narwhal
```

### Nix

```sh
nix run github:Nonanti/narwhal
```

Or add the flake to your inputs and reference the default package.

### Pre-built binaries

Download native tarballs (with SHA-256 checksums) from the
[latest GitHub Release](https://github.com/Nonanti/narwhal/releases):

- `x86_64-unknown-linux-gnu`
- `aarch64-apple-darwin` (Apple Silicon)

Intel Mac users: build from source with `cargo install narwhaldb` for
now; a prebuilt `x86_64-apple-darwin` tarball is on the roadmap once
the macos-13 runner backlog clears.

```sh
curl -LO https://github.com/Nonanti/narwhal/releases/latest/download/narwhal-1.1.0-x86_64-unknown-linux-gnu.tar.gz
tar -xzf narwhal-1.1.0-*.tar.gz
mv narwhal-1.1.0-*/narwhal ~/.local/bin/
```

### Build from source

```sh
git clone https://github.com/Nonanti/narwhal.git
cd narwhal
cargo build --release
# binary at target/release/narwhal
```

## Quick start

> **Upgrading from v1.x?** narwhal 2.0 ships `settings.toml` and
> `connections.toml` schema **v2**. On first launch against a v1
> config you'll see a warning telling you to run
> `narwhal migrate-config`. The migrator writes v2 in place and
> preserves the original at `<file>.v1.bak`. Pass `--dry-run` to
> preview the rewrite, `narwhal config validate` to check the
> current state without touching the disk.

1. **Run `narwhal`.** The TUI opens with an empty editor and a sidebar.
2. **Hit `:add`.** The connection wizard appears. Pick a driver, fill in host + database (or use `:url postgres://user:pass@host/db` to skip the form).
3. **`:open <name>`.** The saved entry connects; the sidebar fills with schemas and tables.
4. **F6 to run.** The whole buffer executes; results appear in the lower pane. Press **F1** any time for the full keymap reference.

### `connections.toml` schema

Named connections live in `~/.config/narwhal/connections.toml`.  One
`[[connection]]` block per database; `[connection.params]` carries the
driver-specific options.  The field names match the
[`ConnectionParams`](./crates/narwhal-core/src/connection.rs) struct —
in particular `username` (not `user`) is the canonical name.

```toml
# Local SQLite — the file path is the only required param.
[[connection]]
id     = "00000000-0000-0000-0000-000000000001"
name   = "smoke"
driver = "sqlite"

[connection.params]
path = "/tmp/narwhal-smoke.db"

# Postgres on a non-default port, no TLS — typical local docker setup.
[[connection]]
id     = "00000000-0000-0000-0000-000000000002"
name   = "demo-pg"
driver = "postgres"

[connection.params]
host     = "127.0.0.1"
port     = 5433
username = "postgres"        # NOTE: `username`, not `user`
password = "narwhal"
database = "demo"
ssl_mode = "disable"         # disable | prefer (default) | require | verify-ca | verify-full
```

File-local drivers (`sqlite`, `duckdb`) tolerate the default `prefer`
so pre-TLS configs still load; the wire layer ignores it.  Network
drivers (`postgres`, `mysql`, `clickhouse`) accept any of the five
`ssl_mode` values plus optional `ssl_root_cert`, `ssl_cert`, `ssl_key`
paths for mutual TLS.

### Connection safety: color, read-only, write confirmation (v1.1)

Three optional fields on `[[connection]]` keep you out of trouble on
production databases:

```toml
[[connection]]
name           = "prod-pg"
driver         = "postgres"
color          = "red"           # red | yellow | green | blue | magenta | cyan
confirm_writes = true            # type YES before any mutating statement
read_only      = true            # syntactic guard rejects non-reads

[connection.params]
# …
```

- **`color`** — the active connection's name is tinted in the status
  bar with the chosen accent. Set `color = "red"` on every production
  connection and "am I on prod?" becomes a glance, not a guess.
- **`confirm_writes`** — before an `INSERT`/`UPDATE`/`DELETE`/DDL
  statement reaches the driver, narwhal opens a modal that previews
  the first statement and demands the user type `YES` exactly. Esc
  cancels. Bare reads run without prompting.
- **`read_only`** — every batch is checked against the same syntactic
  allow-list MCP's `run_query` tool uses (`SELECT`, `WITH`, `SHOW`,
  `EXPLAIN`, `DESCRIBE`, `DESC`, `PRAGMA`, `VALUES`, `TABLE`). Writes
  are rejected before they reach the network.

### `config.toml` (settings)

A `~/.config/narwhal/config.toml` lets you override the renderer
theme and a few editor toggles. Missing fields fall back to their
defaults so a one-line file is enough.

```toml
theme = "dark"           # "dark" (default) | "light" | "high-contrast"

[editor]
tab_width    = 4         # reserved, v1.1 will honour this
use_spaces   = true      # reserved, v1.1 will honour this
line_numbers = true      # reserved, v1.1 will honour this

[keybindings]
vim_mode = true          # reserved, v1.1 will allow opt-out

[diagram]
icons = "ascii"          # "ascii" (default) | "nerdfont"
                         # Only affects the in-TUI diagram modal;
                         # Mermaid / DOT exports always use ASCII.
```

v1.0 wires only the `theme` field; the rest are persisted and
load-validated so the file stays stable across upgrades. The renderer
warns at start-up if the file is malformed instead of silently
falling back to defaults.

### SSH tunnels

Any network connection can prepend an SSH local-port-forward by
adding `ssh` fields to the params block. The forward is opened
implicitly on `:open` and torn down when the session closes.

```toml
[connection.params]
host     = "db.internal"     # resolved on the bastion side
port     = 5432
username = "alice"
database = "prod"

[connection.params.ssh]
host      = "bastion.example.com"
user      = "alice"
port      = 22               # optional — defaults to 22
# key_path  = "~/.ssh/id_ed25519"  # optional — defaults to ssh-agent / ~/.ssh/config
# jump_host = "jump.example.com"   # optional — maps to `ssh -J`
```

The spawned `ssh` subprocess inherits `~/.ssh/config`, the agent,
`Match` blocks, `IdentityAgent`, and FIDO2 keys for free — narwhal
deliberately shells out to OpenSSH rather than embedding its own
client. URL form: `?ssh_host=bastion&ssh_user=alice` on a
`:url postgres://...` invocation.

### TLS defaults changed (v0.2)

**Breaking change:** `ssl_mode = prefer` and `ssl_mode = require` now
perform full CA chain verification instead of accepting any server
certificate. Self-signed certificates will be rejected unless the CA
is explicitly trusted via `ssl_root_cert`.

If you were relying on the previous insecure behaviour:

- **Self-signed servers:** add `ssl_root_cert = "/path/to/ca.pem"` to
  the connection params, or set `ssl_mode = "disable"` if TLS is not
  needed.
- **Hostname mismatch:** use `ssl_mode = "require"` or
  `ssl_mode = "verify-ca"` (chain verified, hostname skipped).
- **Full verification:** `ssl_mode = "verify-full"` (unchanged).

Query-string TLS params (`?sslmode=...`, `?sslrootcert=...`, etc.)
are now parsed into dedicated struct fields instead of being left in
the generic `options` map.

## Diagrams

narwhal builds an entity-relationship diagram out of the same
`describe_table` metadata it already uses for completion and DDL
generation. Three surfaces, one model:

- An in-TUI modal you can open without leaving the editor.
- A `:diagram export` command that hands you Mermaid or Graphviz
  source on the clipboard — paste into mermaid.live, a PR
  description, a Notion page, or pipe through `dot -Tsvg`.
- The `get_diagram` MCP tool so agents see the same picture.

### In-TUI modal

```text
:diagram users               → Focused mode (table + 1-hop neighbours)
:diagram impact users        → Impact mode (reverse-FK closure)
```

When the sidebar pane has focus, `gd` (vim chord) or `D` (single key)
opens the Focused modal on the highlighted table. Inside the modal:

| Keys | Action |
|------|--------|
| Tab / Shift-Tab | Cycle through neighbours |
| j / k / ↑ / ↓ | Same, single-step |
| Enter | Re-centre on the selected neighbour (instant — the model is cached) |
| i | Toggle Focused ↔ Impact |
| y | Yank the current subset as Mermaid to the clipboard |
| g / G / Ctrl-d / Ctrl-u | Scroll |
| Esc / q | Close |

Focused mode shows the centre table as a labelled box (PK / FK / UK
markers) and lists outbound + inbound foreign-key neighbours below
with their FK column and cardinality (`——▶`, `◀——`, `(nullable)`,
`[1‑1]`). Impact mode renders the reverse-FK tree with `ON DELETE`
annotations, flagging `NO ACTION` references that would block a
delete.

The modal uses ASCII markers by default. Set `[diagram] icons =
"nerdfont"` in `config.toml` for Nerd Font glyphs.

### Export to Mermaid / Graphviz

```text
:diagram export mermaid                    → clipboard
:diagram export mermaid ./schema.mmd       → file
:diagram export dot ./schema               → file (.dot extension added)
:diagram export mermaid --table orders     → focused subset
:diagram export mermaid --schema public    → single-schema export
```

Exports always use ASCII markers because Mermaid (mermaid.live) and
Graphviz HTML labels don't reliably ship Nerd Font glyphs; the TUI
is the only surface that opts in. Cardinality is derived from FK
nullability + uniqueness:

| FK columns | Mermaid | Meaning |
|------------|---------|---------|
| NOT NULL, not UNIQUE | `\|\|--o{` | 1-to-many |
| nullable, not UNIQUE | `\|o--o{` | 0..1-to-many |
| NOT NULL, UNIQUE     | `\|\|--\|\|` | 1-to-1 |
| nullable, UNIQUE     | `\|o--o\|` | 0..1-to-1 |

Cross-schema FKs are dropped in V1 so the rendered diagram never
shows dangling edges. Junction tables fall out naturally as two
1-to-many edges.

### Logical relations (FK-less joins)

Micro-service splits, partition pruning, or legacy schemas often
leave behind "this column points at that one" relationships the
engine doesn't enforce. Declare them in TOML and they render
alongside the real FKs — dashed in Mermaid / DOT and tagged `[L]`
in the TUI so you can tell which edges the database actually
guarantees.

```toml
# .narwhal/workspace.toml   (preferred — commit it for your team)
# or ~/.config/narwhal/connections.toml (personal fallback)

[[logical_relation]]
connection  = "prod-db"
from        = "events.user_id"     # [schema.]table.column
to          = "users.id"
cardinality = "many-to-one"        # default: many-to-one
note        = "sharded across regions"
```

Cardinality tokens: `one-to-many`, `many-to-one`, `one-to-one`,
`zero-or-one-to-many`, `zero-or-one-to-one`, `many-to-many`
(kebab-case; `_` and digit forms like `1-to-many` are accepted).

Workspace + connections entries are merged with workspace winning
on duplicates. Entries that reference unknown tables or columns
are dropped with a status-bar warning so a typo in your config
never silently hides the rest of the diagram.

### Limits

- TUI modal renders only the Focused + Impact views — a full
  auto-laid-out overview lives in Mermaid (run
  `:diagram export mermaid` and paste into mermaid.live).
- Cross-schema FKs are not rendered.
- Composite logical relations (multi-column FK-less joins) are
  reserved for V1.1; the `from_columns` / `to_columns` keys are
  accepted by the parser but rejected with a friendly error.

## MCP server: talk to your databases through an AI agent

narwhal ships a built-in [Model Context Protocol](https://modelcontextprotocol.io)
server so any MCP-capable AI assistant (Claude Desktop, Cursor, Continue,
Aider, ...) can browse the connections you already configured and inspect
their schema.

```sh
narwhal mcp   # runs the JSON-RPC stdio server
```

Wire it into Claude Desktop:

```jsonc
// ~/.config/Claude/claude_desktop_config.json
{
  "mcpServers": {
    "narwhal": {
      "command": "narwhal",
      "args": ["mcp"]
    }
  }
}
```

The v0 tool surface:

| Tool | What it does |
|------|--------------|
| `list_connections` | List configured connections — driver, target, SSH flag. No IO, no credentials loaded. Honours the workspace ACL. |
| `describe_schema`  | Schema / table / view tree for one connection. |
| `describe_table`   | Full structure of one table — columns, indexes, foreign keys, unique constraints, engine-native DDL. |
| `run_query`        | Execute a single statement. **Read-only by default** — syntactic guard + `BEGIN/ROLLBACK` sandwich + row limit (default 1 000). `read_only=false` opts out, subject to the workspace ACL. |
| `explain_query`    | Driver-native EXPLAIN with the right dialect prefix. Optional `analyze=true` runs the statement for real cardinalities (PG / MySQL / DuckDB). |
| `get_diagram`      | Render an ER diagram of the connection's schema as Mermaid (`erDiagram`) or Graphviz `dot`. Optional `table` focuses to a 1-hop subset; optional `schema` restricts candidates. Returns a JSON envelope with node/edge counts plus the rendered source. |

Every database-touching call is audit-logged to
`~/.local/share/narwhal/history.jsonl` with `source: "mcp"` so you can
`jq 'select(.source == "mcp")'` to isolate agent traffic.

### Workspace scoping: `.narwhal/workspace.toml`

A repo-local file (discovered by walking up from `pwd`, same idiom as
`.git`) declares what the MCP server may expose when narwhal runs from
inside that directory tree. Commit it next to your code so an agent
launched against your project can only reach the databases you list.

```toml
# .narwhal/workspace.toml

# Connection names from connections.toml that the agent may target.
# Empty / omitted = all of them.
allowed_connections = ["staging", "test"]

# When false, run_query rejects read_only=false. Default true.
allow_writes = false
```

Disallowed connections appear to the agent exactly as a misspelled
name would: the `list_connections` result hides them, `describe_*` /
`run_query` calls answer with the same "unknown connection" tool-level
error, and the agent retries against the visible set automatically.

## Keymap

### Global

| Keys | Action |
|------|--------|
| F5 / Alt-Enter / Ctrl-; | Run statement under cursor |
| F6 | Run whole buffer |
| F7 | Stream cursor statement |
| F4 / Ctrl-C | Cancel running query |
| Ctrl-W | Cycle pane focus |
| Ctrl-T | New editor tab |
| Ctrl-N / :goto / :g | Fuzzy navigator — jump to any table/view |
| :diff `<a>` `<b>` | Schema diff: emit ALTER TABLE migration SQL |
| :lint | Run linter (SELECT *, UPDATE/DELETE no WHERE, cartesian …) |
| :diagram `<table>` | Open the Focused ER diagram modal on a table |
| :diagram impact `<table>` | Reverse-FK closure ("what breaks if I drop this?") |
| :diagram export mermaid\|dot `[path]` | Export schema as Mermaid / Graphviz — to file when a path is given, otherwise clipboard. Add `--table T` / `--schema S` to narrow. |
| :tpl `<name>` | Insert a built-in template: sel / ins / upd / del / join / with |
| :history `[pattern]` | Open Ctrl-R modal (optionally pre-filtered) |
| :submit / :revert | Flush / drop the pending-mutation queue |
| :filter `[expr\|clear]` | Set / clear the result filter |
| :sort `<N\|clear>` | Toggle the result sort on column N |
| :chart `bar\|line\|sparkline` `[--x col] [--y col] [--title T]` | Open an inline ASCII chart over the active result; updates progressively as rows stream in |
| :chart off | Hide the chart pane |
| :pivot `rows=col[,col..] [cols=col] [value=col] [agg=count\|sum\|avg\|min\|max]` | Open an inline pivot table over the active result |
| :pivot off | Hide the pivot pane |
| Alt-N (editor) | Multi-cursor: add a secondary cursor at the next occurrence of the word under cursor |
| Alt-A (editor) | Multi-cursor: add a secondary cursor at every other occurrence in the buffer |
| Esc (editor, multi-cursor active) | Collapse to a single cursor |

#### `:chart` notes

- **Bar** caps the visible series at the top 50 entries ranked by
  absolute magnitude; the rest are dropped, not truncated to a tail.
- **Line** / **Sparkline** keep the last 1000 points (FIFO drop on
  overflow).
- **Sparkline** renders no x-axis labels by design.
- Numeric detection scans the whole row slice; mixed-type columns
  must produce at least one parseable numeric cell.
- Multi-line paste collapses the secondary-cursor set (paste-into-
  multi-cursor lands in v2.1); status bar surfaces a hint.

#### `:pivot` notes

- Distinct values of `cols=` are capped at 50 by default; the
  overflow column is labelled `(other)` and aggregates the rest.
- `count` works on any column; `sum`/`avg`/`min`/`max` require a
  numeric `value=` column.
- Aggregation runs in `f64` — see crate-level `# Precision` note
  for the integer round-off caveat over 2^53.

| Ctrl-Tab / Ctrl-Shift-Tab | Cycle tabs |
| ? / F1 | Help |
| :q | Quit |
| :refresh | Re-fetch schema tree for active connection |

### Editor

| Keys | Action |
|------|--------|
| i / a | Enter insert mode |
| Esc | Back to normal mode |
| Tab / Ctrl-Space | Completion |
| ↑ ↓ / Shift-Tab | Cycle popup items |
| Enter / Tab (in popup) | Accept completion |
| h j k l / arrows | Move cursor |
| w / b | Word forward / backward |
| 0 / $ | Line start / end |
| v / V | Visual / visual-line mode |

### Sidebar

| Keys | Action |
|------|--------|
| j / k / ↑ / ↓ | Navigate |
| Enter | Describe table |
| o | Preview table data |
| d | Inject DDL into editor |
| gd / D | Open ER diagram modal on the selected table |

### Results

| Keys | Action |
|------|--------|
| h j k l / arrows | Move selection |
| Enter | Open cell popup |
| e | Edit cell value |
| y / Y | Yank cell / row to clipboard |
| / | Filter rows |
| n / N | Next / prev search match |
| g / G | Jump to first / last row |
| :next / :prev | Page through results |

### Snippets

| Keys | Action |
|------|--------|
| :save \<name\> | Save editor buffer as a named snippet |
| :load \<name\> | Load a snippet into a new tab |
| :rm-snippet \<name\> | Delete a saved snippet |
| :snippets | Browse saved snippets |

## Plugins

Plugins are Lua scripts that auto-load from `~/.config/narwhal/plugins/*.lua`
(or the platform equivalent under `$XDG_CONFIG_HOME`). They get a `narwhal`
global with these entry points:

```lua
narwhal.register_command(name, description, handler)
    -- handler(arg : string)
    --   return "..."                 -> status bar message
    --   return { sql = "..." }       -> append to editor buffer
    --   return { sql = "...", append = false }
    --                                -> replace editor buffer
    --   return nil | false           -> silent

narwhal.register_transform(handler)
    -- handler(result : table)
    --   mutate in place; return value ignored

narwhal.sql_run(sql : string) -> result
    -- Run SQL on the active connection synchronously

narwhal.editor_text          : string (read-only)
    -- Current editor buffer content during command dispatch
```

### Sample plugins

Six working samples live in [`examples/plugins/`](./examples/plugins/):

| File | What it does |
|------|-------------|
| `uppercase.lua` | Result transform that uppercases every TEXT cell |
| `format_json.lua` | Pretty-prints cells that parse as JSON |
| `row_count.lua` | `:rc <table>` — count rows via `narwhal.sql_run` |
| `query_snippet.lua` | `:top <table>` — inject `SELECT * FROM … LIMIT 10` |
| `csv_export.lua` | `:csv-export <table> <path>` — dump to CSV |
| `explain_cost.lua` | `:explain-cost` / `:explain-sqlite` — wrap buffer in EXPLAIN |

Load on demand: `:plug-load /path/to/file.lua`. List everything: `:plug-list`.

For the full API reference, see [`narwhal-plugin-lua` docs](./crates/narwhal-plugin-lua/src/lib.rs)
and the [plugin examples README](./examples/plugins/README.md).

### Security model

Plugins are **trusted code that runs with your privileges**. They can run
arbitrary SQL, inject into the editor, and read every result row. Only
install scripts from sources you'd trust as a shell script. There is no
sandbox — by design, so auditing a plugin is just reading a short `.lua`
file.

Built-in command names (`run`, `open`, `begin`, `quit`, …) are reserved;
a plugin that tries to shadow one is rejected at load time. During a
`:begin` transaction, `narwhal.sql_run` is refused entirely.

## Headless `exec` mode: one-shot SQL from the shell

The same binary doubles as a `psql -c` / `mysql -e` muadili. Use it in
CI smoke checks, `cron` jobs, or shell pipelines:

```sh
narwhal exec --conn prod 'SELECT count(*) FROM users'

# Choose a format for the consumer (default: table)
narwhal exec -c prod -f json 'SELECT id, email FROM users' | jq '.[].email'
narwhal exec -c prod -f csv  'SELECT * FROM orders' > orders.csv
narwhal exec -c prod -f tsv  'SELECT id, name FROM users' | column -t

# Limit rows to keep large queries snappy
narwhal exec -c prod -l 100 'SELECT * FROM events'

# Writes are sandboxed by default (BEGIN ... ROLLBACK). `--write` opts out:
narwhal exec -c prod --write "UPDATE users SET banned = true WHERE id = 42"
```

Formats: `table` (default, ASCII grid), `csv` (RFC 4180), `json`
(array-of-objects), `tsv` (pipe-friendly, no quoting), `markdown`
(GFM table, truncated at 1000 rows). `parquet` is supported via the
TUI `:export parquet <path>` command — the streaming `exec` sink
cannot own the file footer. Connection
resolution is the same as the TUI — keyring first, `~/.pgpass`/env
fallback. Every call is audit-logged to `history.jsonl` with
`source: "exec"` so it can be grepped alongside MCP traffic.

## Transactions

| Command | Action |
|---------|--------|
| `:begin [iso]` | Open a pinned connection; `iso` accepts `ru`/`rc`/`rr`/`s` short forms |
| `:commit` | Commit and close the pinned connection |
| `:rollback` | Rollback and close the pinned connection |
| `:savepoint NAME` | Create a named savepoint (drivers that support them) |
| `:release NAME` | Release a savepoint |
| `:rollback-to NAME` | Rollback to a savepoint |

A **TX** badge on the status bar reminds you that you're inside a
transaction.

## Architecture

```
              narwhal (bin)
                   │  entry point + CLI
                   ▼
             narwhal-app                       narwhal-mcp
              orchestrator                       MCP server
           │     │      │                          │
           ▼     ▼      ▼                          ▼
   narwhal-tui  narwhal-commands           narwhal-driver-registry
     render     completion / export /             (feature-gated
                wizard / dispatch /                 driver lookup)
                snippets / DDL / …                       │
           │         │                                    ▼
           └─────────┼────────────────┐         narwhal-driver-*
                    ▼                 │          postgres · sqlite
              narwhal-domain          │          mysql · duckdb
              pure model state        │          clickhouse
           (editor buffer, schema     │
            listings, no IO,          │
            no rendering)             │
                   │                  ▼
                   └────────►   narwhal-pool · narwhal-sql ·
                                narwhal-history · narwhal-config ·
                                narwhal-vim
                                       │
                                       ▼
                                  narwhal-core
                            driver trait, value model,
                            schema types, error type
```

Dependency direction: every arrow points downward. View (`narwhal-tui`)
and commands (`narwhal-commands`) read domain state by reference; only
`narwhal-app` is allowed to mutate it in response to user input.
Concrete drivers are reached exclusively through
`narwhal-driver-registry`, so cargo features are the only place where
database engines come and go.

For the full layer map see [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md);
for the code-style contract see [`docs/STYLE.md`](docs/STYLE.md).

## Safety

- Every destructive operation goes through the `:` command line, never a hotkey.
- Statements are journaled to `~/.local/share/narwhal/history.jsonl` before execution.
- Passwords prefer the OS keyring; an in-memory fallback is used only when the keyring isn't available. `:forget <name>` wipes the cached entry.

## Building

### Nix

```sh
nix develop
cargo build --release
```

The dev shell pulls in `cmake`, `clang`, and `libcxx` for the bundled
DuckDB C++ build, and pre-sets `LIBCLANG_PATH` for bindgen.

### Other systems

Any toolchain at or above the version pinned in `rust-toolchain.toml`,
plus the usual native build deps for DuckDB (cmake, a C++17 compiler).

```sh
cargo build --release
```

### Benchmarks

Criterion harnesses live under `crates/*/benches/`. Run them all with
`cargo bench --workspace` or one at a time:

```sh
cargo bench -p narwhal-sql --bench splitter
cargo bench -p narwhal-tui --bench sort
cargo bench -p narwhal-tui --bench editor_motion
cargo bench -p narwhal-history --bench append
```

The pre/post-optimisation numbers are recorded in
[`docs/perf-after-phase-2.md`](./docs/perf-after-phase-2.md); current
headline numbers on a Linux box are ~900 MiB/s for the statement
splitter, ~38 µs for a 5 000-line `w` motion, and ~1.15 ms to sort
2 000 JSON cells.

## Contributing

A few ground rules so PRs land smoothly:

- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and `RUSTDOCFLAGS="-D warnings" cargo doc
  --workspace --no-deps` all pass in CI; please run them locally too.
- New behaviour ships with a regression test under the relevant
  crate's `tests/` directory.
- Commit messages follow Conventional Commits
  (`feat:`, `fix:`, `refactor:`, `docs:`, `chore:`).

## Licence

Dual-licensed under the [MIT](./LICENSE-MIT) and
[Apache 2.0](./LICENSE-APACHE) licences. Contributions are accepted under
the same terms.

---

See [`docs/RELEASING.md`](./docs/RELEASING.md) for the release
checklist and [`CHANGELOG.md`](./CHANGELOG.md) for the version history.
