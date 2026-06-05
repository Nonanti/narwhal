# Group 1 — Foundation Crates Review

## Summary
The four foundation crates are solid overall — `#![forbid(unsafe_code)]` is enforced, `#[non_exhaustive]` is consistently applied to enums and most public structs, the `with(|p|…)` builder pattern is used where needed, error types use `thiserror`, and async hygiene is generally good (MutexGuard dropped before await). The pool crate follows the `pop_fresh_idle` / `spawn_close` conventions from memory. The findings below are things that survived three prior review passes: dependency-direction inversions, a few missing `#[non_exhaustive]` on public data types that are constructed outside their defining crate, a `deny_unknown_fields` forward-compat hazard, redaction coverage gap for MSSQL DSNs, and the `async_trait` inconsistency.

## Critical (P0 — release-blocker class)

### [C1] `narwhal-config` depends on `narwhal-diagram` and `narwhal-audit` — inverted dependency direction
- **File**: `crates/narwhal-config/Cargo.toml:19-20`, `crates/narwhal-config/src/logical_relations.rs:24`, `crates/narwhal-config/src/settings.rs:126`
- **Issue**: `narwhal-config` (a foundation crate) depends on `narwhal-diagram` and `narwhal-audit` (feature-level crates). The intended layering is downward-only: config → core, not config → diagram/audit.
- **Why bad**: Any change to `narwhal-diagram`'s `LogicalRelation`/`QualifiedName` types or `narwhal-audit`'s `AuditConfig` forces a recompile of `narwhal-config`, cascading through every crate that depends on config. Worse, `narwhal-diagram` itself depends on `narwhal-core` — so the chain is `config → diagram → core`, but `config` already depends on `core` directly. The real risk is circular dependency if `diagram` or `audit` ever need config types.
- **Fix**: Extract the shared types into `narwhal-core` or a new `narwhal-config-types` crate. `DiagramIcons` (already duplicated as a mirror in `settings.rs:55`) proves the pattern works — the same duplication should apply to `LogicalRelation`/`QualifiedName`/`Cardinality` (config defines its own `LogicalRelationConfig`, diagram converts at the seam), and `AuditConfig` should be defined in config or core, not in audit. Long term, `narwhal-audit` should consume `AuditConfig` from config, not the other way around.

## Major (P1 — should-fix-before-v2.1)

### [M1] `CredentialStore` uses `async_trait` macro — diverges from workspace convention
- **File**: `crates/narwhal-config/src/credentials.rs:4,45,92,236`
- **Issue**: Per `docs/dev/api-surface.md` and the `Connection`/`DatabaseDriver` traits in `narwhal-core`, the workspace convention is native `async fn` in trait (RPITIT) with a `DynX` sibling for dyn-object sites. `CredentialStore` uses the `async_trait` macro instead, pulling in the `async-trait` crate dependency.
- **Why bad**: Inconsistency forces contributors to remember two async-trait patterns. The `async-trait` macro also produces a `Pin<Box<dyn Future>>` on every call, even for `InMemoryStore` which never needs dyn dispatch. `CredentialStore` IS used as `dyn CredentialStore` (in `resolve_connection_password`), so a `DynCredentialStore` sibling following the core pattern is the right shape.
- **Fix**: Rewrite `CredentialStore` as native `async fn` in trait, add `DynCredentialStore` blanket-impl sibling, remove `async-trait` from `Cargo.toml`. Same pattern as `Connection`/`DynConnection`.

### [M2] `InMemoryStore` uses `std::sync::Mutex` (poisonable) while `Pool` uses `parking_lot::Mutex` (unpoisonable)
- **File**: `crates/narwhal-config/src/credentials.rs:226-227`, `crates/narwhal-pool/src/pool.rs:100`
- **Issue**: `InMemoryStore` wraps its `HashMap` in `std::sync::Mutex`, which can poison and returns `CredentialError::Keyring("lock poisoned")` on every operation. `Pool` uses `parking_lot::Mutex` specifically because it is unpoisonable (the comment at `pool.rs:99-101` calls this out).
- **Why bad**: If any task panics while holding the `InMemoryStore` lock, every subsequent `get`/`set`/`delete` call permanently fails with "lock poisoned". For a credential store this is unnecessarily harsh — the data is still valid. The pool crate already solved this by choosing `parking_lot`.
- **Fix**: Add `parking_lot` to `narwhal-config/Cargo.toml` and switch `InMemoryStore::secrets` to `parking_lot::Mutex`. Remove the `.map_err(|e| CredentialError::Keyring(format!("lock poisoned: {e}")))` calls.

### [M3] `LogicalRelationConfig` has `#[serde(deny_unknown_fields)]` — forward-compatibility hazard
- **File**: `crates/narwhal-config/src/settings.rs:381`
- **Issue**: `LogicalRelationConfig` is the only struct in the entire workspace that uses `deny_unknown_fields`. Every other deserialisable struct allows unknown fields (the serde default), which is the correct choice for forward compatibility.
- **Why bad**: If v2.1 adds a new field (e.g. `on_delete` action, `label`), a v2.0 binary reading a v2.1 config file will reject the entire `[[logical_relation]]` block with "unknown field". This is the exact problem `#[non_exhaustive]` prevents for Rust code — `deny_unknown_fields` creates the same problem for the TOML wire format.
- **Fix**: Remove `#[serde(deny_unknown_fields)]` from `LogicalRelationConfig`. If strictness is desired, it belongs in a `validate` pass, not in deserialization.

### [M4] Redaction regex misses `mssql`/`sqlserver` DSN schemes
- **File**: `crates/narwhal-history/src/journal.rs:72-77`
- **Issue**: The DSN userinfo regex (`r"(?i)\b(postgres(?:ql)?|mysql|clickhouse|redis|mongodb...`) includes `postgres`, `mysql`, `clickhouse`, `redis`, `mongodb`, `jdbc:*` but omits `mssql` and `sqlserver` — both of which the project's URL parser (`narwhal-config/src/url.rs:141-142`) accepts as connection-string schemes.
- **Why bad**: An error message like `"failed to connect: mssql://sa:hunter2@db:1433/master"` would persist the password `hunter2` verbatim in the JSONL journal, bypassing redaction.
- **Fix**: Add `mssql|sqlserver` to the alternation group in the DSN regex.

### [M5] `ConnectionConfig` missing `#[non_exhaustive]` — struct-literal construction is stable API
- **File**: `crates/narwhal-core/src/connection.rs:62`
- **Issue**: `ConnectionConfig` has `pub id`, `pub name`, `pub driver`, `pub params` and is constructed with struct-literal syntax in multiple places across the workspace. But it is not `#[non_exhaustive]`. Adding any field (e.g. `pub group: Option<String>`) is a breaking change.
- **Why bad**: This is the most-frequently-constructed type in the workspace. `ConnectionParams` is correctly `#[non_exhaustive]` with a `with()` builder, but `ConnectionConfig` wraps it and is itself not protected. Per convention `bcad4ja2z4q2a2ckm9mde3p5` (qualified_name_format), the `with()` pattern is the workspace standard for `#[non_exhaustive]` structs.
- **Fix**: Add `#[non_exhaustive]` to `ConnectionConfig` and a `ConnectionConfig::with(|c| …)` builder. Driver code and `ParsedUrl` will need to switch from struct-literal to the builder. This is a breaking change, so it should land early in the v2.1 cycle.

### [M6] `PoolConfig` missing `#[non_exhaustive]` — prevents additive fields
- **File**: `crates/narwhal-pool/src/pool.rs:29`
- **Issue**: `PoolConfig` is a public struct with all pub fields, not marked `#[non_exhaustive]`. Adding `min_idle`, `validation_query`, or any future tuning knob is a semver-breaking change.
- **Why bad**: The pool is a foundation crate consumed by the app layer; breaking changes here ripple everywhere. Every other settings struct in `narwhal-config` and `narwhal-core` is `#[non_exhaustive]` — `PoolConfig` is the exception.
- **Fix**: Add `#[non_exhaustive]` and a `PoolConfig::with(|c| …)` builder. Update test code to use the builder.

## Minor (P2 — nice-to-have)

### [m1] `HistoryEntry` not `#[non_exhaustive]` — struct-literal construction is stable
- **File**: `crates/narwhal-history/src/journal.rs:251`
- **Issue**: `HistoryEntry` is a public struct with `pub` fields and no `#[non_exhaustive]`. Adding a field (e.g. `query_id`) is a breaking change. It already has builder methods (`with_source`, `with_connection`, etc.) but the struct-literal path is also public and used in tests.
- **Fix**: Add `#[non_exhaustive]` and ensure `HistoryEntry::success()` is the sole construction path. Update tests.

### [m2] `ConfigPaths` not `#[non_exhaustive]`
- **File**: `crates/narwhal-config/src/paths.rs:19`
- **Issue**: `ConfigPaths` is `pub struct` with `pub` fields, not `#[non_exhaustive]`. Adding a new path (e.g. `plugin_state_dir`) is a breaking change.
- **Fix**: Add `#[non_exhaustive]`.

### [m3] `ParsedUrl` not `#[non_exhaustive]` and leaks `password` as `Option<String>`
- **File**: `crates/narwhal-config/src/url.rs:28-31`
- **Issue**: `ParsedUrl` has `pub config: ConnectionConfig` and `pub password: Option<String>`. The password is stored as a bare `String`, not `SecretString`. Callers that construct a `ParsedUrl` via struct-literal (if `non_exhaustive` is added, they can't — but currently they can).
- **Fix**: Add `#[non_exhaustive]` and a `ParsedUrl::new()` builder. Consider wrapping `password` in `SecretString` for consistency with the credential resolution path.

### [m4] Stale `once_cell` dependency in `narwhal-history`
- **File**: `crates/narwhal-history/Cargo.toml:26`
- **Issue**: `once_cell = "1"` is listed as a dependency, but the code uses `std::sync::LazyLock` (stable since Rust 1.80, MSRV is 1.85). The `once_cell` crate is never imported — only a stale doc comment at `journal.rs:20` references it.
- **Fix**: Remove `once_cell = "1"` from `Cargo.toml`. Update the comment to say `std::sync::LazyLock`.

### [m5] `SshConfig::new()` not `#[must_use]`
- **File**: `crates/narwhal-core/src/connection.rs:304`
- **Issue**: `SshConfig::new()` returns a constructed value but lacks `#[must_use]`, unlike `ConnectionParams::with()` and `PreConnectStep::new()`.
- **Fix**: Add `#[must_use]`.

### [m6] `Schema`, `Table`, `Column`, `Index`, `ForeignKey`, `UniqueConstraint`, `Row`, `QueryResult`, `ColumnHeader` not `#[non_exhaustive]`
- **File**: `crates/narwhal-core/src/schema.rs:15,29,36,59,74,122,128,149,157`
- **Issue**: These are all public structs with pub fields. Adding a field is a semver break. However, they are also data-bag types constructed by every driver with struct-literal syntax — adding `#[non_exhaustive]` would force every driver to adopt a builder, which is a large mechanical change.
- **Fix**: Defer to v2.1. Add `#[non_exhaustive]` + `with()` builders and migrate drivers in a dedicated PR. The risk is low because these structs represent database catalogue metadata that rarely changes.

### [m7] `InMemoryStore` holds `std::sync::MutexGuard` across an `async_trait` yield point (latent)
- **File**: `crates/narwhal-config/src/credentials.rs:239-242`
- **Issue**: The `async_trait` transformation makes the entire `get`/`set`/`delete` body into a single `async move { … }` block. Currently there are no `.await` calls while the `MutexGuard` is held, so no yield occurs. But a future contributor adding any `.await` inside the lock scope would introduce a `MutexGuard` held across await — a clippy `await_holding_lock` violation and potential deadlock.
- **Fix**: Switching to native `async fn` in trait (M1) makes the yield points explicit. Alternatively, scope the guard explicitly: `let result = { let guard = …; guard.get(&id).cloned() }; Ok(result)`.

### [m8] `Journal` holds `tokio::sync::Mutex` for file writes — potential contention bottleneck
- **File**: `crates/narwhal-history/src/journal.rs:274-276`
- **Issue**: `Journal::file` is a `Mutex<tokio::fs::File>`. Every `append` call acquires the lock, writes, and flushes. Under high-concurrency workloads (e.g. MCP + TUI both writing), this serialises all writes through a single lock. The `concurrent_writes_interleave_at_line_boundaries` test validates correctness but doesn't measure throughput.
- **Fix**: Consider using a `tokio::sync::mpsc` channel with a dedicated writer task so callers never block on I/O. This is a design change, not a correctness fix.

### [m9] `UrlError` implements `std::error::Error` manually rather than via `thiserror`
- **File**: `crates/narwhal-config/src/url.rs:33-76`
- **Issue**: `UrlError` has a manual `impl Display` and `impl std::error::Error`, while every other error type in the foundation crates uses `thiserror` derive. Per convention `fxzxwpuc019bymzzqacnxy8k`, custom error enums should use `thiserror`.
- **Fix**: Replace with `#[derive(Error)]` and `#[error("…")]` attributes.

### [m10] `redact_sql_secrets` doesn't handle `SET SESSION PASSWORD` / `ALTER ROLE … PASSWORD` on PostgreSQL
- **File**: `crates/narwhal-history/src/journal.rs:44`
- **Issue**: The first regex matches `PASSWORD '…'` after `CREATE/ALTER USER` but not `ALTER ROLE admin PASSWORD 'secret'` (PostgreSQL allows `ALTER ROLE` as a synonym). The `\b` before `password` won't match if the keyword is preceded by a non-word char that isn't in the alternation. The pattern `(encrypted\s+)?password\s+` should also match `role_name PASSWORD` (no preceding keyword).
- **Fix**: Extend the first regex to also cover `ALTER ROLE … PASSWORD` — or make the `PASSWORD '…'` pattern standalone (not requiring a preceding keyword), accepting the minor false-positive risk on column-name collisions (already handled by `\b`).

### [m11] `SshTunnel::wait_for_ready` uses `expect()` in non-test production code
- **File**: `crates/narwhal-core/src/ssh.rs:158`
- **Issue**: `.expect("127.0.0.1:<u16> is always a valid SocketAddr")` is used in the `wait_for_ready` method. The workspace convention forbids `unwrap`/`expect` in production code (test-only is fine). While the reasoning is sound (the format is indeed always valid), it violates the project rule.
- **Fix**: Replace with `let addr = format!("127.0.0.1:{}", self.local_port).parse::<SocketAddr>().map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;` — or use a const-expression `SocketAddr` constructor if available.

## Nits (style/doc/naming)

### [n1] Stale doc comment referencing `once_cell::sync::Lazy`
- **File**: `crates/narwhal-history/src/journal.rs:20`
- **Issue**: Comment says "compiled once at first use via `once_cell::sync::Lazy`" but the actual code uses `std::sync::LazyLock`.
- **Fix**: Update comment to say `std::sync::LazyLock`.

### [n2] `HashicorpVaultSettings` address is `Option<String>` but effectively required
- **File**: `crates/narwhal-config/src/settings.rs:328`
- **Issue**: `address` is `Option<String>` with `#[serde(default)]`, but `HashicorpVault::from_settings` immediately errors if it's `None`. The `Option` wrapper gives users a misleading TOML experience — the field appears optional but isn't.
- **Fix**: Document clearly in the doc comment that `address` is required when the `hashicorp` section is present. Or make it a `String` with a custom serde default that fails validation.

### [n3] `Pool` missing `Debug` impl
- **File**: `crates/narwhal-pool/src/pool.rs:73`
- **Issue**: `Pool` has a manual `Clone` derive but no `Debug`. The inner `Inner` struct also lacks `Debug`. Every other public type in the foundation crates derives `Debug`.
- **Fix**: Add `#[derive(Debug)]` to `Pool` (and `Inner` if practical) or implement `Debug` manually with a redacted output.

### [n4] `PooledConnection` missing `Debug` impl
- **File**: `crates/narwhal-pool/src/pool.rs:325`
- **Issue**: `PooledConnection` doesn't implement `Debug`. The `Deref` target is `dyn DynConnection` which is `Debug`-bounded by the trait.
- **Fix**: Implement `Debug` manually.

### [n5] `Journal` missing `Debug` impl
- **File**: `crates/narwhal-history/src/journal.rs:274`
- **Issue**: `Journal` has `path: PathBuf` and `file: Mutex<tokio::fs::File>` but no `Debug` impl.
- **Fix**: Implement `Debug` manually showing only the path.

### [n6] Inconsistent `with()` builder on `LogicalRelationConfig`
- **File**: `crates/narwhal-config/src/settings.rs:381`
- **Issue**: `LogicalRelationConfig` has `#[serde(deny_unknown_fields)]` and is NOT `#[non_exhaustive]` — but it has pub fields that are `Option` with `#[serde(default)]`, which means it's already partially builder-friendly. It should either be `#[non_exhaustive]` with a `with()` builder (like every other config struct) or the `deny_unknown_fields` should be removed.
- **Fix**: Remove `deny_unknown_fields` (M3). Optionally add `#[non_exhaustive]` + `with()`.

### [n7] `ConnectionsFile` not `#[non_exhaustive]`
- **File**: `crates/narwhal-config/src/settings.rs:428`
- **Issue**: `ConnectionsFile` is `pub struct` with pub fields, not `#[non_exhaustive]`. Adding a top-level field (e.g. `default_driver`) would be a breaking change.
- **Fix**: Add `#[non_exhaustive]` and a `with()` builder.

### [n8] `VaultProviderSettings` re-exported but `VaultProvider` trait is intentionally not re-exported — comment could be clearer
- **File**: `crates/narwhal-config/src/lib.rs:18-22`
- **Issue**: The comment explaining why `VaultProvider` (the trait) is not re-exported is good, but the same-name collision between `settings::VaultProvider` (enum) and `vault::VaultProvider` (trait) is confusing for API consumers.
- **Fix**: Consider renaming the enum to `VaultProviderKind` or the trait to `VaultProviderImpl` to eliminate the name collision entirely.

## Strengths
- **Consistent `#[non_exhaustive]` + `with()` builder pattern** across config types — `ConnectionParams`, `VaultSettings`, `VaultProviderSettings`, `HashicorpVaultSettings`, `OnePasswordVaultSettings`, `MigrateOptions` all follow the same convention. This is a copy-worthy pattern.
- **Lock hygiene in `VaultRegistry::resolve`**: The `std::sync::Mutex` guard is explicitly scoped in a block that ends before any `.await`, with a comment explaining why. This is textbook correct.
- **`spawn_close` convention in pool**: Stale/unhealthy connections are closed in background tasks via `tokio::runtime::Handle::try_current()`, so `acquire()` callers are never charged for the round-trip. Falls back to synchronous Drop when no runtime is available. Clean pattern.
- **`pop_fresh_idle` respects both `idle_timeout` and `max_lifetime`**: Expired entries are discarded in the same loop, avoiding two separate scans. The `saturating_duration_since` prevents underflow on clock adjustments.
- **Redaction architecture is thorough**: The two-pass approach (hand-rolled dollar-quote + regex pass) correctly handles PG dollar-quoted function bodies that the `regex` crate can't match due to backreference limitations. The `HistoryEntryView` borrow-based serialisation avoids cloning the entire entry on the redaction cold path.
- **`atomic_write` for config persistence**: Both `Settings::save` and `LastUsedStore::save` use write-then-rename for crash safety, with `0o600` permissions set on Unix before rename. This prevents both partial-write corruption and world-readable config files.
- **Layered credential resolution in `resolve_password`**: The priority chain (vault ref → inline literal → keyring → pgpass/env) is well-documented with explicit fallthrough decisions. The vault-failure-does-not-fall-through-to-keyring invariant is enforced and tested.
- **Dedup broadcast in `VaultRegistry`**: The `broadcast::channel` per in-flight reference is an elegant solution to the "one HTTP call for N concurrent resolves" requirement. Cancellation of a waiter doesn't cancel the leader — correct contract.
