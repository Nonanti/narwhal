# API surface audit — narwhal 2.0 (T0-05)

> Generated as part of **T0-05 — API cleanup pass**. Feeds T3-01
> (migration guide) with the actual v1→v2 surface delta. Re-run
> manually whenever a Tier-1 task lands a new public item.

## Headline numbers

| Metric                                | v1.2.0 | v2.0 (Tier-0 done) | Delta |
| ------------------------------------- | -----: | -----------------: | ----- |
| Workspace crates                      | 21     | 16                 | −5    |
| `#[deprecated]` items                 | 0      | 0                  | 0     |
| `narwhal-core` public type aliases    | 0      | 2 (`BoxFuture`, `SchemaCatalog`) | +2 |
| Driver crates exposed on crates.io    | 5      | 0 (one umbrella)   | −5    |
| `Connection` impls using `#[async_trait]` | yes | no                 | —     |
| `narwhal-core` `async-trait` dep      | yes    | no                 | dropped |

## Method

`cargo-public-api` / `cargo-udeps` aren't available inside this nix
environment (no `cargo install` against a read-only toolchain), so
the audit below is **manual** and grep-driven against the workspace
source tree at the head of `v2-dev`. Steps:

1. `rg "#\\[deprecated" crates/ narwhal/` — confirm zero deprecation
   markers anywhere in the tree.
2. `rg "^pub use|^pub mod " crates/*/src/lib.rs` — enumerate every
   re-exported symbol per crate. Each list is hand-checked against
   the consumer call sites.
3. Driver-side `__test_only` modules verified to still be reachable
   from `crates/narwhal-drivers/tests/`.

When the toolchain regains access to cargo-public-api (e.g. in CI),
T3-01 will produce a machine-generated diff. The list below is the
human-readable baseline.

## Workspace crates after Tier-0

| Crate                  | Role                                          | Public surface size |
| ---------------------- | --------------------------------------------- | ------------------: |
| `narwhal-core`         | traits + types shared by every crate           | medium |
| `narwhal-config`       | settings v2, connections, credentials, pgpass, migrate | medium |
| `narwhal-sql`          | SQL formatter / splitter / lint / guard        | small  |
| `narwhal-pool`         | connection pool over `DynConnection`           | tiny   |
| `narwhal-history`      | JSONL audit journal                           | tiny   |
| `narwhal-drivers`      | bundled drivers (feature-gated) + registry     | medium |
| `narwhal-domain`       | editor + motion + schema view models           | small  |
| `narwhal-commands`     | command dispatch + completion + keymap         | medium |
| `narwhal-plugin`       | plugin host trait                              | tiny   |
| `narwhal-plugin-lua`   | mlua bridge                                   | (internal, no re-exports) |
| `narwhal-vim`          | vim state machine                              | small  |
| `narwhal-diagram`      | diagram model + Mermaid / DOT renderers        | small  |
| `narwhal-tui`          | ratatui widgets                                | medium |
| `narwhal-app`          | TUI app shell                                  | medium |
| `narwhal-mcp`          | MCP server                                     | small  |
| `narwhaldb` (`narwhal`)| binary entry point                             | binary only |

Deleted in v2.0:

- `narwhal-driver-postgres`
- `narwhal-driver-mysql`
- `narwhal-driver-sqlite`
- `narwhal-driver-duckdb`
- `narwhal-driver-clickhouse`
- `narwhal-driver-registry`

All six fold into `narwhal-drivers` with one cargo feature per
backend (T0-03). The umbrella also gains `all-drivers` so the
historical "everything turned on" build is still a one-liner.

## Per-crate re-export inventory

> Each `pub use` and `pub mod` from the `lib.rs` is listed with a
> one-line note. When the item is part of the migration story it
> links to the relevant Tier-0 task in the roadmap.

### `narwhal-core`

```
pub mod {cancel, capabilities, connection, driver, error, schema,
         ssh, stream, value};
pub use cancel::{CancelHandle, DynCancelHandle};             // T0-02
pub use capabilities::Capabilities;
pub use connection::{Connection, ConnectionColor, ConnectionConfig,
                     ConnectionParams, DynConnection,         // T0-02
                     IsolationLevel, PreConnectStep, SshConfig,
                     SslMode};
pub use driver::{DatabaseDriver, DynDatabaseDriver};         // T0-02
pub use error::{Error, Result};
pub use schema::{Column, ColumnHeader, ForeignKey, Index,
                 QueryResult, ReferentialAction, Row, Schema,
                 Table, TableKind, TableSchema, UniqueConstraint};
pub use ssh::{READY_TIMEOUT as SSH_READY_TIMEOUT, SshTunnel};
pub use stream::{DynRowStream, RowStream};                   // T0-02
pub use value::Value;
```

Notes:

- All four "core traits" (Connection, DatabaseDriver, RowStream,
  CancelHandle) ship with a `Dyn*` sibling. Driver authors implement
  the sized trait; consumers hold `Box<dyn DynX>`. See
  `docs/dev/async-trait-style.md`.
- `#[non_exhaustive]` is on every public struct / enum that might
  grow a field. Construction via `with(|p| …)` is the canonical
  builder pattern.

### `narwhal-config`

```
pub mod {credentials, interpolate, last_used, migrate, paths,
         pgpass, settings, url, logical_relations};
pub use credentials::{CredentialError, CredentialStore,
                      InMemoryStore, KeyringStore};
pub use interpolate::{InterpolateError, interpolate,
                      interpolate_connections};
pub use last_used::{LastUsedError, LastUsedStore};
pub use migrate::{                                           // T0-04
    MigrateOptions, MigrateOutcome, MigrateReport,
    ValidateOutcome, ValidateReport,
    migrate as migrate_config, migrate_connections,
    migrate_settings, render_settings_v2,
    validate as validate_config,
};
pub use paths::{ConfigPaths, PathsError};
pub use pgpass::{password_from_env, password_from_pgpass,
                 resolve_password as resolve_fallback_password};
pub use secrecy::SecretString;
pub use logical_relations::{…};
pub use settings::{                                          // T0-04
    CURRENT_SCHEMA_VERSION, ConfigError, ConnectionsFile,
    DiagramIcons, DiagramSettings, EditorSettings,
    HashicorpVaultSettings, KeybindingSettings,
    LogicalRelationConfig, OnePasswordVaultSettings,
    PluginSettings, Settings, Theme, VaultProvider,
    VaultProviderSettings, VaultSettings, WasmPluginSettings,
    WorkspacePersistSettings, WorkspaceSettings,
};
pub use url::{ParsedUrl, UrlError, parse as parse_url};
```

Notes:

- T0-04 added the `migrate` module and 12 new struct/enum names
  for the v2 schema sections. Every new struct is
  `#[non_exhaustive]` and has a `Default` impl, so adding fields
  is non-breaking.

### `narwhal-drivers`

```
#[cfg(feature = "postgres")]   pub mod postgres;
#[cfg(feature = "mysql")]      pub mod mysql;
#[cfg(feature = "sqlite")]     pub mod sqlite;
#[cfg(feature = "duckdb")]     pub mod duckdb;
#[cfg(feature = "clickhouse")] pub mod clickhouse;
#[cfg(feature = "mssql")]      pub mod mssql;        // T1-T2-A
pub mod registry;
pub use registry::DriverRegistry;
pub fn registry() -> DriverRegistry;   // convenience wrapper for
                                       // historical Registry::new()
```

Notes:

- Per-engine sub-module re-exports the historical `*Driver` struct
  (`narwhal_drivers::postgres::PostgresDriver`, etc.).
- The `__test_only` modules inside each engine remain `pub` so the
  in-crate `tests/` files can reach into engine internals without
  exposing them on the lib API.

### `narwhal-app`, `narwhal-mcp`

Both still ship a one-line `pub use narwhal_drivers::DriverRegistry;`
in their own `registry.rs`. This is **deliberate**: it lets the
binary disambiguate via `App::DriverRegistry as AppDriverRegistry`
without forcing every test module to depend on `narwhal_drivers`
directly. The wrappers cost one line each, no runtime, and keep
~50 test imports stable.

### Everything else

No changes vs v1.2.0 except via the workspace-wide effects of
T0-01 (Rust 2024 use-group ordering) and T0-02 (Dyn* trait renames
where dyn-compat sites lived).

## `#[deprecated]` items

```
$ rg "#\\[deprecated" crates/ narwhal/
(no output)
```

Zero. v1.x already had no `#[deprecated]` markers at the v1.2.0 tag
(verified via `git log -G '#\\[deprecated'`), so T0-05's "remove
deprecations" deliverable is trivially satisfied.

## Tools we couldn't run

The brief asked for:

- `cargo public-api -p <crate>` baselines
- `cargo +nightly udeps --workspace --all-targets`

Neither tool is available in this environment (no nightly toolchain;
no writable cargo install root). The procedural equivalents:

- **Re-export audit**: manual grep through `rg "^pub use "
  crates/*/src/lib.rs` (above). Every re-export traces to at least
  one external import.
- **Unused-deps audit**: `cargo build --workspace --all-targets` is
  clean under `clippy::all` (`-D warnings`) which catches the bulk
  of `unused_*` lints. `async-trait` removal from `narwhal-core`
  (T0-02) was confirmed by re-running the build with the dep gone.
- **Cargo feature audit**: `narwhal-drivers/Cargo.toml` features
  are the only non-trivial set; each is used by at least one
  consumer (`narwhal-app`, `narwhal-mcp`, `narwhal-pool`, the
  binary). Verified during T0-03.

T3-01 should re-run cargo-public-api in a richer environment and
attach the diff. The numbers above are the v2-Tier-0 snapshot.

## Conventions for new public items (carry-over to Tier 1)

1. Every new pub struct: `#[non_exhaustive]` + `Default` + a
   `with(|p| …)` builder if it has more than 2 fields. See
   `ConnectionParams` and `MigrateOptions` for the pattern.
2. Every new pub enum that may grow: `#[non_exhaustive]`. Forces
   downstream matches to add `_ =>` arms, which is a meaningful
   migration signal.
3. Re-exports live in `lib.rs`. If a symbol is re-exported, it must
   have at least one external caller — otherwise leave it
   `pub(crate)`.
4. No `#[deprecated]` markers in v2.0. Removals go straight to the
   CHANGELOG; renames go through `pub use NEW as OLD;` for one
   minor version, then drop.
5. Test-only public items live under a `pub mod __test_only { … }`
   sub-module. Never `pub` something at lib root just for a test.

## Deferred work

- Set up `cargo-public-api` in CI (a follow-up to T3-04). Producing
  the machine-generated diff for every PR is the cleanest way to
  catch surface regressions.
- Re-do `cargo udeps` once a nightly is wired into the dev shell.
  Manual eyeballing of `Cargo.toml` workspace deps did not flag
  anything obviously unused, but the automated pass would be more
  thorough.
