# Wave 6 — Wave 5 sonrası kalan iş

Wave 5 (`fix/wave-5-api-cleanup`) API stabilizasyonu + mekanik
temizlik kapsamını kapattı. Aşağıdakiler kasten dışarıda bırakıldı —
hepsi ya yeni feature ya da sözleşmesi tartışılması gereken refactor.

## Açık L bulguları

### L23 — `Tab` field'ları `pub`
**Sebep ertelendi:** `crates/narwhal-app/tests/result_sort_filter.rs`,
`editor_search.rs` vs. integration test'leri `tab.editor`,
`tab.results`, `tab.editor_search` üzerinden doğrudan field okuyor.
`pub(crate)` çekmek **önce** her field için public getter eklemeyi ve
~10 test dosyasını dönüştürmeyi gerektiriyor.

**Plan:**
1. `Tab`'a `editor()`, `editor_mut()`, `results()`, `results_mut()`,
   `editor_search()`, `editor_search_mut()`, `completion()`,
   `page_size()` getter'ları ekle.
2. Tüm integration test'leri getter çağrılarına çevir.
3. Field'ları `pub(crate)` yap (mod sınırı içinde okumayı koru).
4. Hem `cargo test --workspace` hem `cargo clippy -D warnings` yeşil olmalı.

### L24 — Sidebar scroll yok
**Sebep ertelendi:** Yeni feature. > 100 satırlı şemada sidebar
ekrana sığmıyor; clamp ve scroll mantığı eklemek gerek.

**Plan:**
1. `AppCore.sidebar_index` üstüne `sidebar_scroll: usize` ekle.
2. `render_sidebar` callback'ine viewport yüksekliğini geçir;
   `cursor < scroll` ya da `cursor >= scroll + visible` durumlarında
   scroll'u clamp et.
3. Page Up / Page Down + Mouse wheel bindingleri.
4. TUI snapshot test'i (visible_rows < total_rows senaryosu).

### L28 — ClickHouse `query_tsv` tüm gövdeyi `Vec<u8>`'e materialize
**Sebep ertelendi:** Perf, akış mimarisi değişikliği gerekli.
`reqwest::Response::bytes_stream()` + satır-bazlı parser'a geçince
ClickHouse büyük sonuçlar için bellek dostu olur.

**Plan:**
1. `query_tsv` → `query_tsv_stream(...) -> impl Stream<Item = Result<Bytes>>`.
2. `BodyTsvParser` satır birikimini incremental yap.
3. 100M row simülasyonu test'inde max RSS < 256 MiB kalmalı.

### L31 — MySQL `KILL QUERY` cancel desteği yok
**Sebep ertelendi:** Yeni feature. PostgreSQL ve ClickHouse'da
async cancel var; MySQL'de henüz `connection_id` izleme ve
ikinci connection üzerinden `KILL QUERY <id>` yok.

**Plan:**
1. `MysqlConnection`'a `connection_id: Arc<AtomicU64>` ekle, ilk
   `SELECT CONNECTION_ID()` ile doldur.
2. `Connection::cancel_query` impl: ikinci connection aç + KILL QUERY.
3. F4 cancel <100ms hedefi için tokio task'la fire-and-forget.
4. Integration test (uzun `SLEEP(30)` + F4).

## Açık H bulguları

Yok — Wave 5 sırasında H14 da kapatıldı.

## Henüz yapılmamış manuel doğrulamalar

Wave 1-4'ten kalan, runtime ortamı gerektirenler:
- TLS self-signed cert reddi (PG)
- `gcore` parola dump kontrolü
- BIDI Unicode görüntüsü
- F4 cancel <100ms ölçümü
- MySQL Docker entegrasyon koşusu
- ClickHouse cancel davranışı
- DuckDB interrupt davranışı

Bu doğrulamalar smoke-test script'iyle otomatize edilebilir
(`scripts/manual-smoke.sh`).
