# narwhal

A terminal-first database client. Modal editing, multi-engine, scriptable
with Lua, single binary.

## Status

Alpha. The four bundled drivers connect, execute, stream, and introspect
schemas. Every command surfaced by the TUI is unit- or integration-tested
(160+ tests across the workspace) and `cargo clippy --all-targets --
-D warnings` is clean. There is no release pipeline yet — installation is
via `cargo install --path narwhal` or `nix build`.

## What's in the box

### Drivers

| Driver | Streaming | Transactions | Savepoints | Cancellation |
|---|---|---|---|---|
| PostgreSQL | ✓ | ✓ | ✓ | ✓ |
| MySQL      | ✓ | ✓ | ✓ | — |
| SQLite     | ✓ | ✓ | ✓ | ✓ |
| DuckDB     | ✓ | ✓ | — | ✓ |

Adding another engine means writing a new crate that implements the
`DatabaseDriver` and `Connection` traits in `narwhal-core` and registering
it in `DriverRegistry::with_defaults()`. No core changes required.

### Editor & navigation

- True modal input (Normal / Insert / Visual) across every pane, not just
  the SQL buffer. `:`-line commands borrow vim semantics where they
  apply.
- Tab-completion is schema-aware: tables, columns, and keywords come from
  the live connection. Trigger with <kbd>Tab</kbd>.
- Inline cell editing: `e` on a result cell in Normal mode opens the
  value, generates an `UPDATE … WHERE pk = …` statement, and stages it
  for review.
- Yank: `y` copies the focused cell, `Y` the whole row, both as
  tab-separated values into the system clipboard.
- Server-side pagination (`:next` / `:prev` / `:page-size N`) so opening
  a 10M-row table doesn't blow up the buffer.

### Transactions

- `:begin [iso]` opens a pinned connection; subsequent runs all use it.
  `iso` accepts `ru`/`rc`/`rr`/`s` short forms.
- `:commit` / `:rollback` close it.
- `:savepoint NAME` / `:release NAME` / `:rollback-to NAME` for nested
  scopes (drivers that support them).
- A `TX` badge on the status bar reminds you you're inside a transaction.

### Plugins

Lua scripts auto-load from `~/.config/narwhal/plugins/*.lua` (or the
platform equivalent). They get a `narwhal` global with three entry
points:

```lua
narwhal.register_command(name, description, function(arg) ... end)
narwhal.register_transform(function(result) ... end)
local result = narwhal.sql_run("SELECT ...")
```

Commands appear at the `:` prompt; transforms post-process every
row-returning query before it reaches the UI; `sql_run` lets a script
query the active connection. See `examples/plugins/` for four working
samples (uppercase transform, JSON pretty-printer, `:rc <table>` row
count, `:top <table>` snippet).

Load manually with `:plug-load /path/to/file.lua`, list everything with
`:plug-list`.

#### ⚠️ Security model

Plugins are **trusted code that runs with your privileges**. Anything
you drop into the auto-load directory can:

- run arbitrary SQL on every connection you open (via `narwhal.sql_run`);
- inject SQL into the editor;
- read every result row before it reaches you.

Only install scripts from sources you trust as much as you'd trust
running their code as a shell script. There is no sandbox. Auditing a
Lua plugin is just reading the `.lua` file — they're short and
dependency-free on purpose.

Plugins are *also* refused at load time if they try to register a
command name that the built-in parser already claims (`run`, `open`,
`begin`, …) so an override never silently does nothing. And during a
`:begin` transaction `narwhal.sql_run` is refused entirely — a fresh
pool connection wouldn't see the pinned transaction's writes.

#### Limits worth knowing

- `narwhal.sql_run` materialises the whole result set in memory before
  returning to Lua. If your script needs to scan a big table, pass it
  through `LIMIT` or paginate manually — streaming from Lua is a
  future addition, not a current capability.
- Plugin runtimes share a tokio thread pool with everything else.
  A misbehaving plugin can hog a worker but can't deadlock the TUI.

### Safety

- Every destructive operation goes through the `:`-line, never a hotkey.
- Statements are journaled to `~/.local/share/narwhal/history.jsonl`
  before they're executed.
- Passwords prefer the OS keyring; an in-memory fallback is used only when
  the keyring isn't available, and `:forget <name>` wipes the cached
  entry.

## Quick start

```sh
# Once
nix develop                    # NixOS / nix-friendly
cargo install --path narwhal   # everywhere else

# Add a connection
narwhal
:add                           # interactive wizard
:open mydb                     # opens by name
:next, :prev, :page-size 200   # paginate the preview
```

Configuration lives under `~/.config/narwhal/`:
- `connections.toml` — saved connection metadata
- `config.toml` — preferences
- `plugins/*.lua` — auto-loaded plugin scripts

## Architecture

```
narwhal-core              public traits, value model, errors
narwhal-config            on-disk configuration, OS keyring integration
narwhal-sql               dialect-aware SQL helpers
narwhal-pool              async connection pool
narwhal-history           append-only JSONL statement journal
narwhal-driver-postgres   PostgreSQL implementation
narwhal-driver-mysql      MySQL implementation
narwhal-driver-sqlite     SQLite implementation
narwhal-driver-duckdb     DuckDB implementation
narwhal-vim               modal keystroke processor
narwhal-tui               ratatui-based interface
narwhal-plugin            plugin trait + registry (runtime-agnostic)
narwhal-plugin-lua        Lua runtime built on mlua
narwhal-app               event loop, driver registry, terminal lifecycle
narwhal                   binary entry point
```

The split exists so plugin runtimes (today `narwhal-plugin-lua`, in
future a WASM runtime) can stay isolated from the rest of the app and
their chunky dependencies don't leak into every build.

## Building

A `flake.nix` is provided for NixOS hosts:

```sh
nix develop
cargo build --release
```

The dev shell pulls in `cmake`, `clang`, and `libcxx` for the bundled
DuckDB C++ build, and pre-sets `LIBCLANG_PATH` for bindgen.

On other systems any toolchain at or above the version pinned in
`rust-toolchain.toml` will do, plus the usual native build deps for
DuckDB (cmake, a C++17 compiler).

## Licence

Dual-licensed under the [MIT](LICENSE-MIT) and
[Apache 2.0](LICENSE-APACHE) licences. Contributions are accepted under
the same terms.
