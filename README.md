# narwhal

[![CI](https://github.com/berkant/narwhal/actions/workflows/ci.yml/badge.svg)](https://github.com/berkant/narwhal/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#licence)
[![Version](https://img.shields.io/badge/version-1.1.0-brightgreen)](./CHANGELOG.md)
[![Rust 1.75+](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](./rust-toolchain.toml)

> A TUI database client that doesn't feel like the 90s.

![hero](./docs/img/hero.gif)

## Why narwhal

- **One TUI, five databases** — Postgres, MySQL, SQLite, DuckDB, ClickHouse. No driver-juggling, no context-switching between `psql`, `mysql`, and DataGrip.
- **Vim editing + auto-pair + completion** — modal input (Normal / Insert / Visual), schema-aware tab-completion, alias-resolved column hints, and a proper `:` command palette.
- **Lua plugin runtime** — the bits that should be yours, stay yours. Write a `.lua` file, drop it in `~/.config/narwhal/plugins/`, and it's live.
- **SSH tunnels, `~/.pgpass`, OS keyring** — the auth ergonomics you already configured for `psql` work here too. Set `ssh_host=jump.example.com` and the connect path forwards a loopback port for you.

## Install

```
cargo install narwhal
```

### Nix

```sh
nix run github:berkant/narwhal
```

Or add the flake to your inputs and reference the default package.

### Build from source

```sh
git clone https://github.com/berkant/narwhal.git
cd narwhal
cargo build --release
# binary at target/release/narwhal
```

### Pre-built binaries

Download native tarballs (with SHA-256 checksums) from the
[latest GitHub Release](https://github.com/berkant/narwhal/releases):

- `x86_64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

```sh
curl -LO https://github.com/berkant/narwhal/releases/latest/download/narwhal-<version>-x86_64-unknown-linux-gnu.tar.gz
tar -xzf narwhal-*-x86_64-unknown-linux-gnu.tar.gz
mv narwhal-*/narwhal ~/.local/bin/
```

### AUR / Homebrew

Planned for the v1.0 release window — track [packaging issues](https://github.com/berkant/narwhal/labels/packaging) for status.

## Quick start

1. **Run `narwhal`** — the TUI opens with an empty editor and a sidebar.
2. **Hit `:add`** — the connection wizard appears. Pick a driver, fill in host + database (or use `:url postgres://user:pass@host/db` to skip the form).
3. **`:open <name>`** — the saved entry connects; the sidebar fills with schemas and tables.
4. **F6 to run** — the whole buffer executes; results appear in the lower pane. Press **F1** any time for the full keymap reference.

![wizard](./docs/img/wizard.png)

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

### `config.toml` (settings)

A `~/.config/narwhal/config.toml` lets you override the renderer
theme and a few editor toggles. Missing fields fall back to their
defaults so a one-line file is enough.

```toml
theme = "dark"           # "dark" (default) | "light" | "high-contrast"

[editor]
tab_width    = 4         # reserved — v1.1 will honour this
use_spaces   = true      # reserved — v1.1 will honour this
line_numbers = true      # reserved — v1.1 will honour this

[keybindings]
vim_mode = true          # reserved — v1.1 will allow opt-out
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

![completion](./docs/img/completion.png)

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

![help](./docs/img/help.png)

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

## MCP server — talk to your databases through an AI agent

narwhal ships a built-in [Model Context Protocol](https://modelcontextprotocol.io)
server so any MCP-capable AI assistant (Claude Desktop, Cursor, Continue,
Aider, …) can browse the connections you already configured and inspect
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

Every database-touching call is audit-logged to
`~/.local/share/narwhal/history.jsonl` with `source: "mcp"` so you can
`jq 'select(.source == "mcp")'` to isolate agent traffic.

### Workspace scoping — `.narwhal/workspace.toml`

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
name would (the `list_connections` result hides them, `describe_*` /
`run_query` calls answer with the same "unknown connection" tool-level
error) — the agent retries against the visible set automatically.

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
┌─────────────────────────────────────────────────────────┐
│                        narwhal (bin)                     │
│                    entry point + CLI                     │
└───────────────────────┬─────────────────────────────────┘
                        │
┌───────────────────────▼─────────────────────────────────┐
│                     narwhal-app                          │
│          event loop · driver registry · lifecycle        │
├──────────┬──────────┬──────────┬────────────────────────┤
│narwhal-  │narwhal-  │narwhal-  │narwhal-plugin-lua      │
│tui       │vim       │plugin    │  mlua runtime           │
│(ratatui) │(modal    │(trait +  │  └─ scripts via         │
│          │ keys)    │registry) │     narwhal global      │
├──────────┴──────────┴──────────┴────────────────────────┤
│  narwhal-core   ·   narwhal-config   ·   narwhal-pool   │
│  (traits,       ·   (on-disk cfg,    ·  (async conn     │
│   value model,  ·    OS keyring)     ·   pool)           │
│   errors)       ·                    ·                    │
├──────────────────────────────────────┬───────────────────┤
│  narwhal-sql · narwhal-history      │narwhal-driver-*   │
│  (dialect    · (JSONL journal)      │postgres  mysql    │
│   helpers)   ·                      │sqlite   duckdb   │
│              ·                      │clickhouse         │
└──────────────────────────────────────┴───────────────────┘
```

The split exists so plugin runtimes (today `narwhal-plugin-lua`; in
future a WASM runtime) stay isolated from the rest of the app and their
chunky dependencies don't leak into every build. Adding another database
engine means writing a new crate that implements the `DatabaseDriver`
and `Connection` traits in `narwhal-core` and registering it in
`DriverRegistry::with_defaults()` — no core changes required.

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
