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

## Alan A Review — Multi-statement tabs + result lifecycle
**Status:** ✅ Complete  
**Output:** /tmp/review-a-multistatement.md  
**Verdict:** ⚠️ Changes Requested

### Critical (2):
- K1: Tab-switch while query running → run updates land on wrong tab (core.rs:4205–4499, 1508–1516); originating tab stuck in Running forever; multi-statement `swap_remove(0)` corrupts non-active tab's bundle
- K2: INSERT export column/table names unquoted (export.rs:269–277); reserved words (`order`, `from`, `key`, etc.) produce invalid SQL

### High (2):
- Y1: Cancellation detection via `error.contains("cancelled")` string match; non-streaming Cancel → Error (not Cancelled state) due to `streaming: true` guard (core.rs:4283–4308)
- Y2: `extract_source_table` false-positive: `SELECT * FROM orders AS o JOIN users u ON ...` returns `Some("orders")` — wrong table; `AS` not in `is_clause_boundary` list (export.rs:392–401)

### Medium (5):
- O1: `format_count` lacks G/T suffix; 999,999 → "1000.0k" (results.rs:457); test asserts the wrong value
- O2: CSV `\t` not in quoting set (export.rs:141); breaks TSV-mode readers
- O3: `unwrap()` in `refresh_search_matches` at core.rs:2799 (AGENTS.md violation)
- O4: `extract_source_table` doesn't handle quoted identifiers `"schema"."table"` → None (export.rs:432)
- O5: `]r` on single-result silently no-ops — no user feedback (core.rs:4022)

## Alan D Review — TUI / Interaction (Plan 06, DataGrip parity)
**Status:** ✅ Complete  
**Output:** /tmp/review-d-tui.md  
**Verdict:** ⚠️ Changes Requested

### Yüksek (5):
- Y1: Help panel scroll yok — 24-row terminalde ~25 satır clip (Sidebar/Results/Snippets görünmez)
- Y2: `?` help key Mode::Normal guard kısıtıyor — Insert mode'da Results/Sidebar'dan help açılmıyor
- Y3: Auto-pair `--` comment satırında tetikleniyor — comment context check eksik
- Y4: `:s/old/new/g` literal string match (regex değil), `:s/.*/x/g` sessiz başarısız
- Y5: `:s` pattern'de `/` escape edilemiyor — `:s/a\/b/c/` yanlış parse

### Orta (5):
- O1: `yank_cell`/`start_cell_edit` stale `visible_indices` kullanıyor (`open_row_detail` inline recompute'u var, tutarsızlık)
- O2: Dotted completion alias çözülmüyor (`c.` → `customers` kolonu gelmez)
- O3: `cursor_inside_string_literal` SQL `''` escape değil `\` backslash kullanıyor
- O4: `column_cache()` multi-schema key collision, session.column_cache her zaman üstüne yazıyor
- O5: History modal `selected` OOB'da sessiz modal kapanma

### Nit (5):
- N1: `unwrap()` AGENTS.md ihlali core.rs:2799
- N2: Status bar double-space (mode_label formatı)
- N3: `:export <path>` path completion yok
- N4: Cheatsheet `? / F1` yanıltıcı (? koşullu)
- N5: Prompt tab-complete Shift-Tab reverse yok
