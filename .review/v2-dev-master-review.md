# narwhal v2-dev — Master Code Review

**Branch:** `v2-dev` (v2.0.0 released)
**Date:** 2026-06-05
**Method:** 5 paralel `rust-dev` subagent fan-out (Foundation / Drivers / App+TUI / SQL Stack / Plugin+Network)
**Hygiene baseline:** `fmt ✓` | `clippy --all-targets -D warnings ✓` | `rustdoc -D warnings ✓` | `test: 1484 passed / 0 failed / 33 ignored ✓`

---

## TL;DR

Codebase **release-ready**. Hijyen mükemmel, üç review pass'i sonrası bulgular incelmiş. Kritik 4 madde var ama hiçbiri "release rollback" sınıfı değil — hepsi v2.0.x patch ile kapatılabilir. Major sınıfı ~15 madde, çoğu v2.1 öncesi temizlenmeli. Mimari sağlam, plugin/sandbox/audit sistemleri production-grade.

**En öncelikli iş:** README'yi v2.0'a güncelle (release-readiness gap) + 4 Critical madde için patch.

---

## 🔴 Critical (P0 — v2.0.x patch için aday)

### [C1] DuckDB `read_only=true` connect-time'da yok sayılıyor
- **File:** `crates/narwhal-drivers/src/duckdb/mod.rs:92-132`
- **Issue:** Config'de `read_only = true` ayarlanmış olsa bile DuckDB driver `access_mode='READ_ONLY'` connection-string parametresini eklemiyor; `set_read_only()` ise `Error::Unsupported` döndürüyor. Sonuç: sessiz yazılabilir bağlantı.
- **Other drivers comparison:** MSSQL `AtomicBool` guard, PG/MySQL/SQLite engine-level — sadece DuckDB gap.
- **Fix:** `connect()` içinde `params.read_only` true ise path'e `?access_mode=READ_ONLY` ekle.

### [C2] ClickHouse IPv6 host URL parse hatası
- **File:** `crates/narwhal-drivers/src/clickhouse/mod.rs:348`
- **Issue:** `format!("{scheme}://{host}:{port}/")` → `https://::1:8123/` (RFC 3986 ihlali). IPv6 host'a bağlanmaya çalışan kullanıcı `Error::Config` alır.
- **Fix:** `if host.contains(':') { format!("[{host}]") }` ile bracket'le.

### [C3] PG/MySQL/SQLite/DuckDB/ClickHouse → error source chain kaybı
- **File:** 5 driver'da ~60+ site. Örnek: `postgres/mod.rs:108,117,249,863,883,959,967`, `mysql/mod.rs:116,123,...`, vs.
- **Issue:** `Error::Connection(e.to_string())` → driver-specific error tipini source zincirinden atıyor. `find_source::<tokio_postgres::Error>()` çalışmaz. Sadece MSSQL `connection_with`/`query_with` kullanıyor.
- **Why bad:** Downstream retry logic, error classification, debugging için engine error tipine erişim yok.
- **Fix:** Mekanik refactor — `Error::connection_with("pg connect", e)` ve `Error::query_with("pg query", e)`. Pattern MSSQL'de örnek.

### [C4] `narwhal-config` foundation crate'i **feature crate'lere** depend ediyor (ters yön)
- **File:** `crates/narwhal-config/Cargo.toml:19-20`, `settings.rs:126`, `logical_relations.rs:24`
- **Issue:** `narwhal-config` → `narwhal-diagram` + `narwhal-audit`. Yön: foundation ↔ feature ters. `narwhal-diagram` zaten `core`'a depend ediyor, yani zincir `config → diagram → core` ama config zaten core'a direkt depend.
- **Risk:** Cycle riski (diagram/audit ileride config tipi gerektirirse). Şu an çalışıyor ama mimari kokuyor.
- **Fix:** `LogicalRelation`/`Cardinality`/`QualifiedName` ve `AuditConfig` tiplerini `narwhal-core`'a (veya yeni `narwhal-config-types`'a) taşı. `DiagramIcons` zaten settings.rs'de duplike — pattern çalışıyor.
- **Severity note:** Critical olarak işaretlendi ama "release rollback" değil — bu **v2.1 mimari fixup** sınıfı. Şimdi acele etmiyor, ama v2.1'de yapılmalı.

---

## 🟠 Major (P1 — v2.1 öncesi düzeltilmeli)

### Foundation
- **[M1.1]** `CredentialStore` `async_trait` macro kullanıyor — workspace native `async fn`/RPITIT konvansiyonundan sapma. `crates/narwhal-config/src/credentials.rs:4,45,92,236`
- **[M1.2]** `InMemoryStore` `std::sync::Mutex` poisonable; `parking_lot::Mutex`'e geç (pool zaten kullanıyor). `credentials.rs:226-227`
- **[M1.3]** `LogicalRelationConfig` `#[serde(deny_unknown_fields)]` — TOML wire format forward-compat hazardı (v2.1 yeni field eklerse v2.0 binary reject eder). Workspace'te tek örnek. `settings.rs:381`
- **[M1.4]** Redaction regex `mssql|sqlserver` DSN schemes'lerini içermiyor → `mssql://sa:hunter2@db/` parolası journal'a sızabilir. `narwhal-history/src/journal.rs:72-77`
- **[M1.5]** `ConnectionConfig` `#[non_exhaustive]` değil — workspace'te en çok struct-literal ile kurulan tip. Yeni field ekleme breaking. `narwhal-core/src/connection.rs:62`
- **[M1.6]** `PoolConfig` `#[non_exhaustive]` değil. `narwhal-pool/src/pool.rs:29`

### Drivers
- **[M2.1]** MySQL + ClickHouse `ssl_cert` set, `ssl_key` boş (veya tersi) → sessiz fall-back to non-mTLS (PG bunu reddediyor). Security regression. `mysql/mod.rs:176`, `clickhouse/mod.rs:211`
- **[M2.2]** SQLite + DuckDB `close()` → `Ok(())` döndürüyor ama `Arc<Mutex<Connection>>` içindeki connection'ı drop etmiyor. SQLite için file lock cleanup gecikir. `sqlite/mod.rs:623-625`, `duckdb/mod.rs:830-832`
- **[M2.3]** TUI sessions.rs:355 yorumu yalan: "set_read_only(true) at session open". Aslında sadece MCP context çağırıyor — TUI'de sadece syntactic guard, MSSQL hariç hiçbir driver connect-time'da read-only engine'e basmıyor.

### App + TUI
- **[M3.1]** Vim `gg` (file start) motion yok. `G` (file end) var. Standard vim user beklentisi. `crates/narwhal-vim/src/machine.rs`
- **[M3.2]** Pivot derive O(rows × cols) per render — 100K+ row streaming sonuçlarda her frame'de full recompute. Incremental accumulator gerek. `crates/narwhal-pivot/src/lib.rs`

### SQL Stack
- **[M4.1]** Parquet schema-infer row 100 sonrası tip değişimi → SESSİZ DATA LOSS (null'a düşürür, warning yok). `crates/narwhal-commands/src/export/parquet.rs:47-50, 234`
- **[M4.2]** MySQL emitter `MODIFY COLUMN ... /* keep existing type */` → kullanıcı blind çalıştırırsa SQL syntax error. Comment column-def pozisyonunda invalid. `schema-diff/src/emit/mysql.rs:138-142`
- **[M4.3]** MSSQL emitter synthesized `df_<table>_<col>` constraint name auto-named (DF__users__email__3B75D7A0) constraint'le eşleşmez → DROP CONSTRAINT runtime fail. Header comment uyarıyor ama hala foot-gun. `schema-diff/src/emit/mssql.rs:130-135, 188-194`
- **[M4.4]** Type normalisation: `int` → `int4` mapping eksik (sadece `integer` → `int4`). Phantom diff. `schema-diff/src/normalise.rs:35-45`
- **[M4.5]** Type normalisation: precision-qualified `timestamp(0) without time zone` synonym matching yapmaz. Phantom diff. `normalise.rs:41`
- **[M4.6]** `defaults_equal` `::type` cast suffix (PG: `'foo'::text`) strip etmiyor → phantom diff. `normalise.rs:82-92`
- **[M4.7]** Eski `schema_diff.rs` (single-table) vs yeni `narwhal-schema-diff` crate'i tip karşılaştırmasında tutarsız (eski raw string compare, yeni canonical_type). `narwhal-commands/src/schema_diff.rs:62-68`

### Plugin / Network
- **[M5.1]** WASM sandbox `Operation::FsRead/FsWrite/NetConnect/EnvRead` variantları public surface'ta ama hiçbir host fn onları construct etmiyor (T1-T5-B deferred). Audit consumer "denial" görür ki gerçek WASM trap'inden gelmez. Surface'in "wired vs not-wired" durumu doc'ta net olmalı. `crates/narwhal-plugin-wasm/src/sandbox/operation.rs:17-30`
- **[M5.2]** `narwhal-lsp` crate **orphan** (app/binary wire değil). Public surface gerçek sqls/sqlls'e karşı stres test edilmemiş. `CompletionResponse::Array` vs `List` variant'ları integration test'siz. v2.1 wiring breaking riski. `crates/narwhal-lsp/`

---

## 🟡 Minor (P2 — backlog)

### Foundation
- `HistoryEntry`, `ConfigPaths`, `ParsedUrl`, `Schema/Table/Column/Index/FK/Unique/Row/QueryResult/ColumnHeader` (9 type), `ConnectionsFile` — hepsi `#[non_exhaustive]` değil. Schema-family için defer-to-v2.1 önerisi (mekanik migration costly).
- `narwhal-history` `once_cell` dep eski (kod `std::sync::LazyLock` kullanıyor). Stale.
- `SshConfig::new()` `#[must_use]` yok.
- `Journal::file` `tokio::sync::Mutex<File>` → high-concurrency write throughput; mpsc-based writer task daha iyi.
- `redact_sql_secrets` `ALTER ROLE … PASSWORD` (PG synonym) regex'e girmiyor.
- `SshTunnel::wait_for_ready` `.expect("always valid SocketAddr")` — prod expect convention ihlali. `crates/narwhal-core/src/ssh.rs:158`
- `UrlError` manuel `Display`/`Error` impl — `thiserror` kullanılmalı.

### Drivers
- PG `quote_ident` iki yerde duplicate (`mod.rs:255`, `ddl.rs:13`).
- MySQL + MSSQL `BufferedRowStream` duplicate — shared module'a çıkar.
- ClickHouse `escape_sql_string` iki yerde duplicate.
- ClickHouse `substitute_params` ve `replace_question_marks` near-duplicate; biri delegate etmeli.
- DuckDB `INSERT … RETURNING` detection var, ClickHouse'da yok (24.x destekliyor).
- PG `Type::CHAR_ARRAY` `Value::String`'e map — muhtemelen `BPCHAR` ile karışmış.
- Port=0 hiçbir driver'da reddedilmiyor.
- `__test_only` modülleri `#[cfg(test)]` gated değil — non-test binary'de yer kaplıyor.

### App + TUI
- `swap_remove(0)` AllDone intermediate state fragile (şu an early-return yok).
- Per-pid persist fallback file'lar asla cleanup edilmiyor.
- Mouse scroll cursor'u hareket ettiriyor (viewport değil) — standart terminal scroll davranışı değil.
- Vim visual mode count prefix (`3j`) ignore.
- Vim yank/delete operator action'ları parse ediliyor ama `apply_action` no-op.
- Chart `--bound N` flag yok — magic 50/1000 sabit.
- Page size 0 reddedilmiyor (LIMIT 0 query).
- Treesitter reparse incremental edit delta'ya beslenmemiş (full reparse her sefer).
- Sidebar Enter `describe_table` event loop'ta inline (>30ms freeze riski).

### SQL Stack
- Missing system schemas: `pg_temp`, `INFORMATION_SCHEMA` (upper), `backup` (MSSQL). `schema-diff/src/diff.rs:70-78`
- Case-insensitive column matching PG case-sensitive identifier collapse'üne neden olabilir. `diff.rs:126-131`
- Backtick splitter MySQL doubled-backtick escape (`` `a``b` ``) handle etmiyor. `splitter.rs:210-215`
- Multi-line `SELECT *` (SELECT ile `*` ayrı satırda) lint kaçırıyor.
- `SQLITE_DEFAULT_CHANGED` rebuild comment `{:?}` Debug format kullanıyor (`Some("0")` yerine `0`).
- Generic emit `CREATE INDEX name ON  (cols)` double space (empty table arg).
- CSV UTF-8 BOM emitmiyor (Excel uyumu için config olabilir).
- Keymap: `spc`, `spacebar` alias yok; `+` key bindable değil.

### Plugin / Network
- `TransformErrors` `pub Vec<String>` field — `#[non_exhaustive]` + private + accessor.
- `narwhal-plugin-lua::from_path` symlink canonicalize etmiyor (trust boundary doc).
- Per-Store `tokio::sync::Mutex` WASM call'larını serialize ediyor (high-freq event plugin için ceiling).
- Audit file sink explicit `0640` permissions yok (default umask'a güveniyor).
- MCP `run_in_sandbox` `run_query.rs` ve `explain_query.rs`'de duplicate.
- LSP `ClientHandle` `Debug` yok.
- `narwhal-domain::SchemaListing` type alias — newtype'a evrilebilir.
- `narwhal-domain::floor_char_boundary` `pub` ama sadece internal kullanım, `pub(crate)` olmalı.

---

## 🟢 Strengths (referans pattern'lar)

1. **`#[non_exhaustive]` + `with(|p|...)` builder pattern** — config/core'da tutarlı uygulanmış (ConnectionParams, VaultSettings, MigrateOptions, HashicorpVaultSettings vs).
2. **Pool: `pop_fresh_idle` + `spawn_close`** — idle_timeout ve max_lifetime tek scan'de; spawn-close fail-back-to-Drop pattern temiz.
3. **Vault `broadcast::channel` per in-flight ref** — N concurrent resolve, 1 HTTP call. Cancellation correctness.
4. **Layered credential resolve** — vault → inline → keyring → pgpass/env; vault-fail-no-fallthrough invariant testli.
5. **MSSQL statement classifier** — comment-aware, literal-aware, CTE/OUTPUT/EXEC routing. Tek driver-layer read-only guard.
6. **TLS hardening** — PG `InternalSslMode` map, custom `VerifyCaNoHostname`, ClickHouse `danger_accept_invalid_certs = false` baseline.
7. **ClickHouse streaming** — chunked TSV decoder, byte-level field split, invalid-UTF-8→Bytes preservation, RAII QueryGuard, bounded channel backpressure, mid-row truncation detection.
8. **Audit `Notify` shutdown** — Sender::closed deadlock'tan kaçınıyor; block-mode `rx.close()` ile emitter'lar `SendError::Closed` görüyor. Idempotent.
9. **Audit rotation atomic** — `create_new` ile claim, ms-resolution stamp + `-N` collision suffix, MAX 1024 attempt. Drop-handle correct (no /dev/null placeholder).
10. **MCP collision-safe tool registration** — builtin > dynamic > first-wins; descriptor `Cow<'static, str>`; central response cap 512KiB.
11. **MCP run_query BEGIN/ROLLBACK sandwich + driver-fallback** — transactionless driver'larda bare execute fall-through; read-only enforcement belt-and-suspenders.
12. **Lua sandbox** — `StdLib` explicit bit-or (yeni mlua version'larında silent leak yok), registry-stored timeout budget (script tamper edemez), `spawn_blocking` + `block_in_place` bridge.
13. **WASM sandbox** — Engine/Linker shared Arc; per-plugin HostState (KV namespace + capability set + fuel budget); StoreLimitsBuilder 64MiB cap; KV 256KiB cap; trap-on-deny via `wasmtime::Error::msg`.
14. **Dispatch reducer** — `AppCore::dispatch` tek girdi, mutation/side-effect ayrımı temiz; `meta_channel` async + tab_id staleness guard; `run_tab_index` resolve-at-start prevents mid-run tab-switch corruption.
15. **State decomposition** — AppDeps (immutable services) / ModalState (overlays) / SessionState / UiState / ProcessState ayrımı.
16. **Workspace persist** — atomic temp+rename, 0o600, lock + per-pid fallback, OOB clamp, ts_parser/sql_highlights deliberately not persisted (raw C ptr).
17. **Treesitter scope API** — kind/statement_range/clause_range immutable contract — T2-T3-C LSP ve T2-T3-D multi-cursor read-only consume.
18. **Lint architecture** — comment-strip + paren-depth + dialect-aware splitter + CTE prefix skipper; INSERT...SELECT exemption for select-star.
19. **Multi-cursor MVP** — sorted Vec<(usize,usize)> + binary_search insert + dedup + primary-coincidence filter; multi-line paste collapses to single (documented scope).
20. **Driver consistency** — 6 driver aynı yapı: `mod.rs` + `types.rs` + opsiyonel `tls.rs`/`ddl.rs`; feature-gate cleanliness (`cargo tree --no-default-features` minimal).

---

## Önerilen patch sırası

### v2.0.1 patch (release-blocker olmayan ama acil)
1. **C1** DuckDB read-only connect-time
2. **C2** ClickHouse IPv6 URL bracket
3. **M1.4** Redaction mssql/sqlserver
4. **M2.1** MySQL/ClickHouse mTLS both-or-neither
5. **M4.1** Parquet silent data loss → en azından `tracing::warn!`
6. **README** v2.0 update (v1.1 atıklarını temizle, eksik özellikleri tanıt)

### v2.1 sprint
1. **C3** Error source chain (5 driver, mekanik)
2. **C4** narwhal-config → narwhal-{audit,diagram} dep inversion
3. **M1.1-M1.6** Foundation non_exhaustive + async_trait elimination + parking_lot
4. **M2.2** SQLite/DuckDB close() guard.take()
5. **M3.1** Vim `gg` + visual count + yank/delete wire
6. **M3.2** Pivot incremental accumulator
7. **M4.2-M4.7** Schema-diff dialect fixes + normalise extensions
8. **M5.1-M5.2** WASM op variant doc + LSP integration test

### v2.2+ backlog
- Per-pid persist cleanup
- Treesitter incremental edit delta
- Schema-family `#[non_exhaustive]` mass migration
- Streaming Parquet writer
- LSP editor wiring

---

## Detaylı raporlar
Her grubun tam çıktısı (file:line referansları, "Why bad" / "Fix" detayları):
- G1 Foundation — `.review/g1-foundation.md`
- G2 Drivers — `.review/g2-drivers.md`
- G3 App + TUI — `.review/g3-app-tui.md`
- G4 SQL Stack — `.review/g4-sql-stack.md`
- G5 Plugin + Network — `.review/g5-plugin-network.md`
