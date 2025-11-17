# Progress

## Status
In Progress

## Tasks

## Files Changed

## Notes

## Alan C Review — TLS / Cancellation / Plugin Runtime (Network + Sandbox)
**Status:** ✅ Complete  
**Output:** /tmp/review-c-network.md  
**Verdict:** ⚠️ Changes Requested

### Critical (3):
- K1: Postgres `insecure_client_config` drops mTLS silently for `ssl_mode=require` — `with_no_client_auth()` always used, `let _ = params` placeholder in prod code
- K2: ClickHouse `ssl_root_cert` loaded but bypassed by `danger_accept_invalid_certs(true)` for Prefer/Require — user gets false sense of CA pinning
- K3: `Duration::from_secs_f64` panics for `set_timeout(1e20)` — inside spawn_blocking → opaque JoinError

### High (4):
- Y1: MySQL partial mTLS (ssl_cert but no ssl_key) silently skipped — no validation parity with Postgres
- Y2: ClickHouse Identity applied even with ssl_mode=Disable (plain HTTP)
- Y3: MySQL VerifyCa == VerifyFull (hostname check not skipped for verify-ca)
- Y4: Lua transform path has no timeout — only command dispatch is guarded

### Medium (4):
- O1: Postgres VerifyCa==VerifyFull design decision undocumented to users
- O2: ClickHouse Certificate::from_pem only parses first cert in bundle (vs rustls full bundle)
- O3: set_timeout_extends_budget uses os.clock() (CPU) vs Instant (wall) — CI flaky risk
- O4: Keyring test doesn't verify D-Bus runtime availability

## Alan B Review — Schema Metadata + DDL Generation
**Status:** ✅ Complete  
**Output:** /tmp/review-b-schema.md  
**Verdict:** ⚠️ Changes Requested

### Critical (3):
- K1: `postgres/ddl.rs:75-97` — GENERATED ALWAYS AS IDENTITY / GENERATED STORED columns silently emit wrong DDL (`DEFAULT nextval(...)` instead of identity syntax) — semantically breaks column on reapplication
- K2: `sqlite/lib.rs:504` — `type='table'` filter silently drops view DDL; sidebar shows views but `d` returns "DDL not found"
- K3: `duckdb/lib.rs:666` — `duckdb_tables()` excludes views; same silent failure as K2

### High (2):
- Y1: `core.rs:3493,3503` — AtomicBool `Ordering::Relaxed` for cross-thread store+swap; not safe on ARM/POWER; should be Release/Acquire
- Y2: `snippets.rs:64-66` — `std::fs::write()` non-atomic; crash mid-write leaves zero-byte snippet file; should use write+rename

### Medium (4):
- O1: `postgres/ddl.rs:51-59` — NOT NULL + PRIMARY KEY + DEFAULT ordering is non-idiomatic (correct by PostgreSQL parser, but pg_dump produces different order; PRIMARY KEY implies NOT NULL)
- O2: No `:rm-snippet` confirmation guard — accidental permanent deletion
- O3: `sqlite/lib.rs:504` — double-quote used for string literal comparison (SQLite quirk fallback), should use single-quote or parameter
- O4: `clickhouse/lib.rs:829-835` — dead-code `nth(1)` fallback + misleading two-column comment

### Test Coverage Gaps:
- Postgres DDL: no composite PK, JSONB, ARRAY, DEFAULT now(), identity, or computed column tests
- DuckDB/SQLite: no view DDL test
