# Group 2 — Drivers Review

**Scope**: `crates/narwhal-drivers/` — 10 191 LOC, 6 drivers, registry, types, DDL, TLS.
**Date**: 2026-06-05
**Reviewer**: Subagent pass (3 passes — structural, semantic, edge-case)

---

## Summary

The crate is well-structured, consistently documented, and has clearly benefited from multiple prior fixup rounds (T1-T2-A classifier, TLS hardening, RETURNING-clause detection, sql_variant CAST, read-only guard). The code compiles clean with `--all-features`, no `openssl-sys` or `native-tls` leaks, and the `cargo tree --no-default-features` footprint is minimal. Test coverage is solid for binding, type coercion, and streaming; docker-gated integration tests are properly ignored.

That said, three classes of issue survive the prior passes:

1. **Source-chain loss** in PG/MySQL/SQLite/DuckDB/ClickHouse error mapping — `Error::Connection(e.to_string())` discards the driver error, making downstream `find_source::<T>()` impossible. Only MSSQL uses `connection_with`/`query_with`.
2. **DuckDB read-only gap** — `params.read_only` is silently ignored at connect time; the driver neither opens with `access_mode='READ_ONLY'` nor calls `set_read_only(true)`.
3. **ClickHouse IPv6 URL construction** — `format!("{scheme}://{host}:{port}/")` breaks on IPv6 literal hosts.

No unsafe code, no unwrap/expect in production paths, no resource leaks in the happy path. Close() implementations correctly drop resources.

---

## Critical

### [C1] ClickHouse `build_base_url` produces invalid URLs for IPv6 hosts

- **File**: `crates/narwhal-drivers/src/clickhouse/mod.rs:348`
- **Issue**: `Url::parse(&format!("{scheme}://{host}:{port}/"))` generates `https://::1:8123/` for an IPv6 loopback host, which is not a valid authority. RFC 3986 requires IPv6 literals to be bracketed: `https://[::1]:8123/`.
- **Why bad**: Any user connecting to a ClickHouse instance on an IPv6 address (dual-stack hosts, k8s services, localhost `::1`) gets `Error::Config("invalid URL: ...")`. The error message doesn't hint at the bracket requirement.
- **Fix**: Bracket the host when it contains `:`:
  ```rust
  let host_part = if host.contains(':') { format!("[{host}]") } else { host.to_owned() };
  Url::parse(&format!("{scheme}://{host_part}:{port}/"))
  ```

### [C2] DuckDB ignores `ConnectionParams::read_only` at connect time

- **File**: `crates/narwhal-drivers/src/duckdb/mod.rs:92-132`
- **Issue**: The `connect` method opens the DuckDB file without checking `config.params.read_only`. The `set_read_only` method returns `Error::Unsupported` with a helpful hint about `access_mode='READ_ONLY'`, but the hint is never acted upon automatically. A user who sets `read_only = true` in their config file gets a writable DuckDB connection with no warning.
- **Why bad**: Every other driver (PG, MySQL, SQLite, ClickHouse, MSSQL) enforces read-only either at the engine level or via the driver-layer guard from connect time. DuckDB is the sole gap. The `duckdb` crate supports connection-string parameters — `?access_mode=READ_ONLY` — but the driver never constructs one.
- **Fix**: When `config.params.read_only` is true, construct the connection string as `path?access_mode=READ_ONLY` (or the equivalent API for `duckdb::Connection::open_with_flags`). Alternatively, call `set_read_only(true)` after connect and log the `Unsupported` error at warn level (matching the MCP pattern).

### [C3] Error source chain lost for PostgreSQL, MySQL, SQLite, DuckDB, ClickHouse

- **File**:
  - `crates/narwhal-drivers/src/postgres/mod.rs:108,117,249,863,883,959,967`
  - `crates/narwhal-drivers/src/mysql/mod.rs:116,123,249,259,...` (20+ sites)
  - `crates/narwhal-drivers/src/sqlite/mod.rs:105,240,...` (10+ sites)
  - `crates/narwhal-drivers/src/duckdb/mod.rs:128,332,...` (10+ sites)
  - `crates/narwhal-drivers/src/clickhouse/mod.rs:236,247,412,427,...` (15+ sites)
- **Issue**: All five drivers convert engine-specific errors to `Error::Connection(e.to_string())` or `Error::Query(e.to_string())`, which discards the original error from the source chain. MSSQL is the only driver that uses `Error::connection_with()` / `Error::query_with()` to preserve the underlying cause.
- **Why bad**: `narwhal_core::Error::find_source::<tokio_postgres::Error>()` and similar downcasts are impossible. The `ErrorWithSource` variants exist specifically for this purpose (see `error.rs` M1 comment). Any downstream debugging, retry logic, or error classification that needs the concrete engine error type is forced to parse the string representation.
- **Fix**: Migrate error-mapping functions to use `Error::connection_with(msg, error)` and `Error::query_with(msg, error)`. This is a mechanical change — each `map_err(|e| Error::Connection(e.to_string()))` becomes `map_err(|e| Error::connection_with("pg connect", e))` etc. The string-only variants can remain for non-error cases (e.g. `"connection closed"`).

---

## Major

### [M1] MySQL and ClickHouse silently accept `ssl_cert` without `ssl_key` (and vice versa)

- **File**:
  - `crates/narwhal-drivers/src/mysql/mod.rs:176`
  - `crates/narwhal-drivers/src/clickhouse/mod.rs:211`
- **Issue**: The PostgreSQL driver correctly enforces the both-or-neither rule: if `ssl_cert` is set without `ssl_key` (or vice versa), it returns `Error::Config("ssl_cert is set but ssl_key is missing; both must be provided together")`. The MySQL and ClickHouse drivers use `if let (Some(cert), Some(key)) = (...)` which silently skips mTLS setup when only one half is present, connecting without client auth — no error, no warning.
- **Why bad**: A misconfigured mTLS setup silently degrades to a non-mTLS connection. The user believes they are using client certificates but the server never sees them. In production environments that require mTLS, this is a security regression.
- **Fix**: Add the same both-or-neither check after the `if let` block in both drivers, mirroring the PostgreSQL pattern:
  ```rust
  if config.params.ssl_cert.is_some() != config.params.ssl_key.is_some() {
      return Err(Error::Config("ssl_cert and ssl_key must both be provided or both omitted".into()));
  }
  ```

### [M2] SQLite and DuckDB `close()` silently drops connections without cleanup

- **File**:
  - `crates/narwhal-drivers/src/sqlite/mod.rs:623-625`
  - `crates/narwhal-drivers/src/duckdb/mod.rs:830-832`
- **Issue**: Both drivers' `close()` implementation is `Ok(())` without actually taking the connection out of the `Arc<Mutex>` or dropping it explicitly. The `MysqlConnection::close()` correctly does `guard.take()` and calls `disconnect()`. The `MssqlConnection::close()` does `guard.take()`. SQLite and DuckDB just return `Ok(())` and rely on the `Box<Self>` drop to eventually clean up.
- **Why bad**: When `close()` is called, the caller expects the connection to be torn down immediately. The `Arc<Mutex<Connection>>` inside the struct keeps the connection alive until all `Arc` references drop. If the streaming task or a `spawn_blocking` closure still holds a clone of the `Arc`, the connection stays open indefinitely. For SQLite this matters because the file handle (and WAL lock) persists until the `Connection` drops.
- **Fix**: Take the connection out of the mutex in `close()`, mirroring the MySQL/MSSQL pattern:
  ```rust
  async fn close(self: Box<Self>) -> Result<()> {
      let mut guard = self.inner.lock().await;
      guard.take(); // Drop the Connection explicitly
      Ok(())
  }
  ```

### [M3] PostgreSQL `cancel_handle` always returns `Some(...)` even in Disable TLS mode, contradicting capabilities

- **File**: `crates/narwhal-drivers/src/postgres/mod.rs:893-908`
- **Issue**: The `cancel_handle()` method returns `Some(Box::new(PostgresCancelHandle { ... }))` regardless of TLS mode. When TLS is disabled, the handle creates a `tls_factory` closure that always returns `Err`, but the actual `cancel()` method uses `NoTls` directly — so cancellation *does* work. However, the construction of a dead closure is confusing and the `CancelHandle` trait semantics suggest that returning `Some` means cancellation is available.
- **Why bad**: Not a functional bug (cancellation works via `NoTls`), but the dead `tls_factory` closure that returns `Err` is misleading. A reviewer would reasonably assume the closure is called somewhere and that cancellation is broken in Disable mode. The `Arc::new(|| Err(...))` allocation is also wasted.
- **Fix**: Simplify: always return `Some(...)`, and in the cancel handle's `cancel()` method, just use `NoTls` for `InternalSslMode::Disable` (which is already the case). Remove the dead closure branch.

### [M4] ClickHouse `cancel_handle` reads but does not drain active query IDs

- **File**: `crates/narwhal-drivers/src/clickhouse/mod.rs:1195-1200`
- **Issue**: The `ClickhouseCancel::cancel()` method clones all active query IDs, issues `KILL QUERY WHERE query_id IN (...)`, but then leaves the IDs in the active set. The `QueryGuard` RAII pattern is supposed to remove them, but there's a race: the KILL may succeed before the query's HTTP response arrives, so the streaming task may still be running and holding the guard. The cancel test explicitly verifies that query IDs remain after cancel.
- **Why bad**: After a successful cancel, the same query IDs remain in the active set. If the user calls `cancel()` again (e.g. double-tap), it issues a second `KILL QUERY` for IDs that may already be dead. Not harmful but wasteful. More importantly, the `KILL QUERY` response may arrive before the original query's response, but the original query's `QueryGuard` drop is spawned on a tokio task that may not have run yet — so a subsequent cancel on the same connection could re-kill an already-cancelled query.
- **Fix**: After issuing KILL QUERY, remove the killed query IDs from the active set immediately (before the streaming task's guard has a chance to clean up). The guard's Drop should also check if the ID is still present before removing (idempotent).

---

## Minor

### [m1] PostgreSQL `map_pg_error` uses string-only `Error::Query` — source chain lost

- **File**: `crates/narwhal-drivers/src/postgres/mod.rs:249`
- **Issue**: `map_pg_error` maps to `Error::Query(error.to_string())`. The `Cancelled` case is correctly detected, but all other server errors lose the `tokio_postgres::Error` source, preventing `find_source` from working.
- **Fix**: Use `Error::query_with("postgres query failed", error)` for the non-Cancelled path.

### [m2] MySQL `with_conn` closure shape is complex — could use a simpler pattern

- **File**: `crates/narwhal-drivers/src/mysql/mod.rs:404-415`
- **Issue**: The `with_conn` helper takes a closure that returns `Pin<Box<dyn Future<Output = Result<R>> + Send + 'a>>`, which is the most complex signature in the crate. Every call site must wrap in `Box::pin(async move { ... })`.
- **Why bad**: Readability. The same pattern in MSSQL (`with_client`) has the same shape. Both could potentially use a macro or a different design (e.g. releasing the mutex guard before calling into the driver).
- **Fix**: Not blocking — just a future refactor candidate. Consider a macro that reduces boilerplate.

### [m3] ClickHouse `substitute_params` and `replace_question_marks` are near-duplicates

- **File**: `crates/narwhal-drivers/src/clickhouse/mod.rs:430-490, 503-540`
- **Issue**: `substitute_params` handles both `?` and `$N` placeholders, while `replace_question_marks` only handles `?`. The two functions share identical quote-tracking and char-walking logic. The `__test_only` module exposes `replace_question_marks` for integration tests.
- **Why bad**: Divergent maintenance risk — a fix to quote-tracking in one must be applied to the other.
- **Fix**: Have `replace_question_marks` delegate to `substitute_params` (which is a superset), or remove it if no external caller requires the `$N`-free variant.

### [m4] DuckDB and SQLite streaming tasks hold `Arc<Mutex<Connection>>` for the entire query

- **File**:
  - `crates/narwhal-drivers/src/sqlite/mod.rs:295-395`
  - `crates/narwhal-drivers/src/duckdb/mod.rs:395-500`
- **Issue**: The `spawn_blocking` closure in `stream()` clones `Arc<Mutex<rusqlite::Connection>>` and calls `blocking_lock()` for the entire duration of the query. This means no other operation (ping, begin, close) can proceed while the stream is open.
- **Why bad**: This is intentional (SQLite is single-writer), but the same pattern is used for DuckDB which supports concurrent readers. The `execute()` path also holds the mutex for the full query, so streaming is no worse — but it does block all other connection methods for the duration of the result set.
- **Fix**: Low priority. Document the trade-off in the stream method's doc comment.

### [m5] No port-zero validation across any driver

- **File**: All six `build_*_config` / `connect` functions
- **Issue**: `ConnectionParams.port` is `Option<u16>`. A user setting `port = 0` would pass it to the underlying driver, which would either fail with an obscure OS error or (in the case of MSSQL/PG) silently use the default. No driver validates that the port is non-zero.
- **Fix**: Add a `if let Some(0) = config.params.port { return Err(Error::Config("port must be > 0")) }` check in each driver's `connect`.

### [m6] ClickHouse `statement_returns_rows` doesn't detect `INSERT … RETURNING` (unlike DuckDB)

- **File**: `crates/narwhal-drivers/src/clickhouse/mod.rs:378-385`
- **Issue**: The DuckDB driver detects `INSERT … RETURNING`, `UPDATE … RETURNING`, etc. via `has_returning_clause()`. The ClickHouse driver's `statement_returns_rows()` only matches `SELECT | WITH | SHOW | DESCRIBE | EXPLAIN | EXISTS`. ClickHouse does support `INSERT … RETURNING` (since 24.x), but the heuristic would route such statements through `execute_raw`, discarding returned rows.
- **Why bad**: Users running `INSERT INTO … RETURNING *` on ClickHouse get `rows_affected: None, columns: [], rows: []` instead of the returned data.
- **Fix**: Port the DuckDB `has_returning_clause` logic (already quote-aware and word-boundary-aware) to the ClickHouse driver's `statement_returns_rows`.

### [m7] PostgreSQL `CHAR_ARRAY` type maps to `Value::String` — should be `Vec<String>`

- **File**: `crates/narwhal-drivers/src/postgres/types.rs:82`
- **Issue**: The `Type::CHAR_ARRAY` match arm produces `Value::String`. This is the `_char` array type in PostgreSQL. While arrays in general are not modeled in `Value` (they fall through to the text fallback), `CHAR_ARRAY` being the only array type in the match is suspicious — it was likely added for `bpchar` (blank-padded character) but `_char` (the array) is a different OID.
- **Fix**: Replace `Type::CHAR_ARRAY` with `Type::BPCHAR` in the match arm. `CHAR_ARRAY` should fall through to the text/unknown fallback like all other array types.

### [m8] TUI comment claims `set_read_only(true)` is "applied at session open" but it isn't

- **File**: `crates/narwhal-app/src/core/sessions.rs:355`
- **Issue**: The comment says "The driver-side `set_read_only(true)` call (applied at session open) is the second layer." but only the MCP context actually calls `set_read_only(true)` after connect. The TUI relies solely on the syntactic `guard_read_only()` check, and only MSSQL's driver-layer `AtomicBool` guard is enforced from connect time. PG, MySQL, SQLite, and ClickHouse connections opened with `read_only = true` in the TUI are not actually in engine-level read-only mode — a SQL injection or UI bug could bypass the syntactic check and execute writes.
- **Fix**: Either (a) add a `set_read_only(true)` call after connect in the TUI session-open path (matching the MCP pattern), or (b) have each driver's `connect()` method check `params.read_only` and apply the guard automatically (as MSSQL already does).

---

## Nits

### [n1] PostgreSQL `quote_ident` is defined twice

- **File**: `crates/narwhal-drivers/src/postgres/mod.rs:255` and `crates/narwhal-drivers/src/postgres/ddl.rs:13`
- **Issue**: Identical `quote_ident` function duplicated between `mod.rs` and `ddl.rs`.
- **Fix**: Make `mod.rs`'s `quote_ident` `pub(super)` and use it from `ddl.rs`.

### [n2] MySQL `__test_only` module exposes internal functions — consider `#[cfg(test)]` gating

- **File**: `crates/narwhal-drivers/src/mysql/mod.rs:12-28`
- **Issue**: The `__test_only` module is always compiled (not gated by `#[cfg(test)]`), exposing `try_value_to_my`, `value_from_my`, etc. Same pattern in DuckDB and MSSQL. These are used by the integration test files in `tests/`.
- **Fix**: Consider using a `#[cfg(test)]` or a `tests` feature to gate these. The current approach works because the module is `#[doc(hidden)]` and not part of the public API, but it bloats the non-test binary.

### [n3] MSSQL `BufferedRowStream` duplicates MySQL's `BufferedRowStream`

- **File**: `crates/narwhal-drivers/src/mysql/mod.rs:804-820` and `crates/narwhal-drivers/src/mssql/mod.rs:1482-1498`
- **Issue**: Identical struct with identical `RowStream` impl. Both buffer a materialised `QueryResult` and replay rows from an `IntoIter`.
- **Fix**: Extract to a shared `crate::buffered_row_stream` module.

### [n4] DuckDB `format_column_type` is `fn` but could be `const fn`

- **File**: `crates/narwhal-drivers/src/duckdb/mod.rs:199-242`
- **Issue**: All match arms return `String` from `&str` via `.into()` or `format!()`, preventing `const fn`. The simple cases could use `const fn` if they returned `&str`.
- **Fix**: Not worth the refactor — just noting it.

### [n5] ClickHouse `escape_sql_string` is defined twice

- **File**: `crates/narwhal-drivers/src/clickhouse/mod.rs:546-557` and `crates/narwhal-drivers/src/clickhouse/types.rs:253-264`
- **Issue**: Two identical `escape_sql_string` functions.
- **Fix**: Use one (the one in `types.rs`) from `mod.rs`.

### [n6] Inconsistent identifier quoting style across drivers

- **Files**: All six driver `mod.rs` files
- **Issue**: Each driver defines its own `quote_ident` with the appropriate quoting style (`""` for PG/SQLite/DuckDB/ClickHouse, `` ` `` for MySQL, `[]` for MSSQL). This is correct per-engine, but the function signature is `fn quote_ident(name: &str) -> String` everywhere — a shared trait or at least a shared doc comment linking the conventions would improve discoverability.
- **Fix**: Low priority. Add a `/// Quoting convention: <engine-specific>` doc comment to each.

### [n7] ClickHouse `list_tables` uses string interpolation for the schema name

- **File**: `crates/narwhal-drivers/src/clickhouse/mod.rs:906-910`
- **Issue**: `format!("... WHERE database = '{}' ORDER BY name", escape_sql_string(schema))` interpolates the schema name into the SQL string. While `escape_sql_string` prevents injection, the ClickHouse driver has a working parameter-substitution system (`substitute_params` + `$N`). Using string interpolation for one query and parameter binding for others is inconsistent.
- **Fix**: Use the `?` or `$1` placeholder pattern with `query_tsv`'s parameter substitution, matching the pattern used in `describe_table`.

---

## Strengths

1. **Consistent architecture** — Every driver follows the same structural pattern: `mod.rs` (driver + connection + row stream + cancel), `types.rs` (bidirectional conversion), optional `tls.rs` / `ddl.rs`. New driver authors can copy any one and be productive.

2. **Feature gate cleanliness** — `cargo tree --no-default-features` pulls in only `narwhal-core`, `tokio`, and `tracing`. No `openssl-sys`, no `native-tls`, no `libduckdb`, no `libsqlite3`. Each driver's optional dep chain is well-isolated.

3. **TLS hardening** — The PostgreSQL driver's `InternalSslMode` mapping, the custom `VerifyCaNoHostname` verifier that only swallows `NotValidForName`, and the consistent `danger_accept_invalid_certs = false` across ClickHouse represent a genuinely secure-by-default posture.

4. **MSSQL statement classifier** — The `classify_statement` / `contains_top_level_keyword` / `leading_keyword` trio is well-engineered: comment-aware, literal-aware, CTE-aware, OUTPUT-aware, and word-boundary-safe. The comprehensive test suite locks in every edge case.

5. **Read-only enforcement depth** — The two-layer approach (syntactic guard + driver-layer guard for MSSQL, engine-level for PG/MySQL/SQLite/ClickHouse) is sound. The MSSQL `AtomicBool` pattern is particularly clean — initialized from config, toggleable at runtime, checked before every execute.

6. **ClickHouse streaming** — The chunked TSV decoder with byte-level field splitting, TSV escape decoding, invalid-UTF-8 preservation as `Value::Bytes`, RAII `QueryGuard`, and backpressure via bounded channels is production-quality. The truncation-mid-row detection is a nice touch.

7. **Prepared statement caching** — PostgreSQL's LRU-prepared-statement cache (64 entries, `std::sync::Mutex`) avoids repeated prepare round-trips for the schema introspection queries that fire on every sidebar refresh.

8. **Test quality** — Every driver has unit tests for capabilities, config validation, type coercion, and statement classification. The ClickHouse stream tests with chunked TSV decoding, binary string preservation, and truncation detection are exemplary. DuckDB's `RETURNING` clause detection tests prevent the exact regression that existed before.
