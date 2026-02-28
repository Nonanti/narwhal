# narwhal — v1.1 / v1.2 Yol Haritası

> DataGrip'ten ödünç alınacak yüksek-ROI özellikleri terminal-yerli biçimde
> narwhal'a entegre etme planı. Öncelik = (kullanıcıya değer) / (efor).
> Tahminler tek geliştirici, parça başı odaklı çalışma varsayımıyla.

## Tema ayrımı

- **v1.1 — "Navigation & Safety"**: yön bulma + üretim güvenliği.
  Yeni hiçbir driver yok; tamamen TUI + commands + sql katmanı.
- **v1.2 — "The Data Editor"**: row-editor'ın hak ettiği hali. FK
  navigation, modify-table, schema diff. Asıl "DataGrip in terminal"
  iddiasını burada karşılıyoruz.
- **v1.3 — "Polish"**: linter, live templates, history search,
  notebook tab'ları. Sıkıcı ama günlük UX'i hisseden işler.

Kapsam dışı (kasıtlı): görsel ER diagram editörü, drag-drop query builder,
embedded LLM completion. Ürünün kimliği = MCP + Lua + vim; oraya yatırım.

---

## v1.1 — Navigation & Safety (≈ 2 hafta)

### 1. `:goto` — fuzzy schema navigator  *(P0, 3–4 gün)*

**Ne:** Ctrl-N / `:goto` → tüm bağlı schema'lardaki tablo, view, kolon,
fonksiyon, sequence üzerinde fuzzy ara. Enter = sidebar'ı o objeye scroll
+ açık tab'a `SELECT * FROM <table>` ya da kolon insert.

**Neden:** DataGrip kullanıcısının kas hafızasının %60'ı. Sidebar'ı
fareyle gezmenin terminalde karşılığı yok; bu boşluğu kapatır.

**Nasıl:**
- Yeni crate yok. `nucleo-matcher` (Helix de kullanıyor, hafif, no_std-friendly).
- Veri: `narwhal-domain::SidebarModel` zaten flat liste üretebiliyor;
  `iter_searchable_items()` ekle (kind, qualified_name, parent_ref).
- UI: `core/state/modals.rs` içine `GotoModal { query, results, cursor }`.
- Komut: `commands.rs`'e `Command::Goto`, alias `:g`, varsayılan keybind `Ctrl-N`.
- Hedef tip başına aksiyon:
  - Table/View → sidebar'ı seç + status bar'da DDL hint
  - Column → editor'ün cursor'una `<table>.<column>` insert
  - Function → editor'a signature insert

**Bitti tanımı:** 10k+ obje olan bir schema'da <16 ms ilk sonuç, key
event'leri 60 fps drop etmiyor.

---

### 2. Connection color-coding & write guards  *(P0, 1–2 gün)*

**Ne:** `connections.toml`'a iki opsiyonel alan:
```toml
[[connection]]
name    = "prod-pg"
color   = "red"              # red | yellow | green | blue | magenta | cyan
confirm_writes = true        # UPDATE/DELETE/DROP için onay iste
read_only      = false       # true = MCP gibi sandbox sandwich + UI'da yazma blok
```

**Neden:** "Yanlış sekmede UPDATE çalıştırdım" insidans → 0. Tek başına
50 satır kod, kazancı orantısız büyük. DataGrip'in en sevilen küçük fikri.

**Nasıl:**
- `narwhal-config::ConnectionParams`'a `color: Option<ConnectionColor>`
  + `confirm_writes: bool` + `read_only: bool` ekle.
- `narwhal-tui` çerçevesi: aktif connection'ın rengi varsa pane border'ı
  + status bar'ın sol kısmı o renkle çizilsin (ratatui `Block::border_style`).
- `narwhal-app/core/dispatch.rs`: Run yolunda
  - `read_only=true` → mevcut `guard_read_only` (mcp'den taşı `narwhal-sql`'e!)
  - `confirm_writes=true` + mutating statement → `:` palette modal "type YES"
- `narwhal-sql`'e yeni modül `guard.rs`: `classify_statement(sql) -> Kind`
  (Read | Write | DDL | Tx). MCP de aynı kodu kullanacak; bugün duplicate.

**Bitti tanımı:** prod sekmesinde `DELETE` yazıp F6 basınca onay modal'ı,
"NO" → çalıştırmıyor; ekran kenarı kırmızı çerçeve hep görünür.

---

### 3. EXPLAIN visualizer  *(P1, 3–5 gün)*

**Ne:** `:explain` çıktısını ASCII ağacı + cost bar olarak çiz. PG için
`EXPLAIN (FORMAT JSON, ANALYZE, BUFFERS)`, MySQL için JSON tree, DuckDB
için tree. ClickHouse `EXPLAIN PIPELINE`. Hot node kırmızı, "estimate
×10 sapma" sarı, "Seq Scan + Filter on large table" mavi badge.

**Neden:** Ham EXPLAIN okumak insanları yıldırıyor; pgMustard / DataGrip'in
plan visualizer'ı tek başına bir araç olarak satılıyor. Terminalde
karşılığı yok — narwhal bunu kapsa diferansiyatör olur.

**Nasıl:**
- Yeni crate: `narwhal-explain` (no_std-leaning, sadece serde + types).
- Driver-başına parser (PG JSON şeması en stabil olan).
- `narwhal-tui`'ye `ExplainPane` widget: indent + `║`/`╠` çubuklarıyla
  ağaç, sağda `cost / actual time / rows (est→act)` üç sütun.
- `:explain analyze` zaten var → çıktı tipini `Text` yerine `Plan` döndür.
- Klavye: `j/k` ile node gez, `Enter` → o node'un detay sheet'i
  (filter, output cols, buffers).

**Bitti tanımı:** 50+ node'lu bir PG query plan'ı tek ekrana sığıyor,
hot path renkli, Enter ile detay.

---

### 4. `guard_read_only`'i ortaklaştır  *(P0, yarım gün — temizlik borcu)*

`crates/narwhal-mcp/src/tools/run_query.rs::guard_read_only` ve
yan fonksiyonlar (`strip_sql_literals`, `contains_word`,
`strip_leading_comments_and_whitespace`) → `narwhal-sql::guard` modülüne
taşı. MCP ve TUI aynı denylist'i paylaşsın. Şu anki duplikasyon
v1.1'deki "connection read-only" işine engel.

---

## v1.2 — The Data Editor (≈ 3–4 hafta)

> Bu sürüm proje için en kritik. README'deki "DataGrip in your terminal"
> iddiasını ya burada karşılarız, ya da iddiayı geri çekeriz.

### 5. Row editor — pending changes UI  *(P0, 1 hafta)*

**Ne:** Result pane'de:
- `i` → satır insert (yeni boş satır)
- `dd` → satır delete (pending)
- Cell üzerinde `c` / `Enter` → düzenle
- `:diff` → tüm pending değişiklikleri SQL olarak preview
- `:submit` → tek transaction'da uygula
- `:revert` → tümünü at
- Editor başlığında `[3 pending]` rozeti, satır başında `+` / `~` / `-` işaretleri

**Neden:** `cell_edit.rs` + `pending.rs` zaten var ama UI ham. DataGrip'in
ana kullanım modu = "tabloyu aç, satırı düzelt, commit". Bu olmadan
proje "okuma odaklı" kalır.

**Nasıl:**
- `narwhal-domain::ResultModel`'e `PendingMutations` ekle (insert/update/delete map).
- `narwhal-commands::pending`'i kullanıcı yüzeyine çıkar: yeni komutlar.
- DDL üreteci: PK'sı olan tablolarda `WHERE pk = ...`; PK yoksa düzenlemeyi reddet ve
  "tablo PK eksik — :force ile ROWID/CTID üzerinden yaz" mesajı.
- Driver başı pk introspection metodu zaten var (`describe_table`).
- Test: PG + SQLite + MySQL üçü için fixture + integration test.

**Risk:** ClickHouse'un `MutationsMergeTree`'si farklı semantik;
ilk sürümde ClickHouse'da row-editor disable, "use ALTER TABLE UPDATE" hint.

---

### 6. FK navigation — `gd` / `gr`  *(P0, 3 gün)*

**Ne:**
- Result hücresinde, kolon FK ise `gd` (go-to-definition) →
  referans tabloyu o değer için filtreli aç (`SELECT * FROM <parent> WHERE pk = <value>`).
- Tersi: `gr` (go-to-references) → "bu satırı referans veren tablo/satırlar".
  Birden çok varsa, küçük picker.

**Neden:** DataGrip "Go to Referenced/Referencing" — veri arkeolojisinin
en sık ihtiyacı. SQL elle yazmaktan kullanıcıyı kurtarır.

**Nasıl:**
- `narwhal-driver-*::describe_table` zaten FK'leri döndürüyor → cache'le.
- `narwhal-app::core::results_actions`'a `goto_fk(cell)` + `find_refs(row)` ekle.
- Yeni tab açma stratejisi: aynı connection'da yeni result-tab, history'ye yazılsın.

---

### 7. Inline filter & sort bar  *(P1, 3–4 gün)*

**Ne:**
- Result başlığında `/`  → kolon adı + operatör + değer (ör. `status = 'open'`).
  Editör tarafına dokunmadan, sunucuya yeni query gönderir.
- Kolon başlığında `s` → ORDER BY o kolon (toggle ASC/DESC/none).
- `:filter clear` → tüm filtreleri sıfırla.

**Neden:** Kullanıcının LIMIT 1000 sonuca tutturup `/` ile aramaya
çalıştığını çok gördük. Server-side filter doğru olan.

**Nasıl:**
- `narwhal-sql` AST'i tam yok; basit `WHERE` ifadesi append etmek için
  splitter zaten var. SELECT'in son `WHERE`/`ORDER BY` clause'larını
  parse etmek gerekiyor — minimal recursive parser veya `sqlparser-rs`
  bağımlılığını ekle (deny.toml'da review et).
- UI: sütun bar bir filter pop-up; ratatui form widget.

---

### 8. Schema diff & `:alter` DDL üretici  *(P1, 1 hafta)*

**Ne:**
- `:diff schema <conn-a> <conn-b>` → iki bağlantının şemaları arasında
  migration SQL üret (basit: kolon ekle/sil/type değiştir; index ekle/sil).
  Karmaşık (constraint reorder, partitioning) için "manual review needed".
- Sidebar tabloda `:alter` → form: yeni kolon ekle (name, type, nullable,
  default), kolon rename, drop. Preview SQL → `:submit`.

**Neden:** `dump-schema` tek yönlü; migration üretimi geliştiricilerin
elle yaptığı sıkıcı iş.

**Nasıl:**
- `narwhal-commands::ddl` zaten var, oraya `diff` ve `alter_table` builder ekle.
- Driver başına quoting + tip mapping farklı; ortak `DdlBuilder` trait'i tanımla,
  her driver implement etsin (PG `BIGSERIAL`, MySQL `BIGINT AUTO_INCREMENT`...).
- Test: snapshot test (insta) ile her driver için beklenen DDL.

---

## v1.3 — Polish (≈ 2 hafta)

### 9. SQL inspections (linter)  *(P1, 4–5 gün)*

Editör altında inline uyarı:
- `SELECT *` production write-context'inde (info, opt-out)
- Cartesian product (FROM a, b … JOIN şartı yok)
- `UPDATE`/`DELETE` `WHERE`'siz (autocommit'te kritik, transaction'da uyarı)
- Bilinmeyen identifier (sidebar şeması ile cross-check)
- Shadowed alias, kullanılmayan CTE
- Driver-spesifik anti-patternler (MySQL `SELECT FOR UPDATE` non-PK ile)

**Nasıl:** Sayet `sqlparser-rs` v1.2'de eklendiyse yeniden kullan;
yoksa lightweight tokenizer. Sonuçlar `ratatui::Span`'larla underline + status
bar mesaj. `:lint off`, `:lint <id> off` ile bastırma.

### 10. Live templates  *(P2, 2–3 gün)*

`sel<Tab>` → `SELECT $cols$ FROM $table$ WHERE $cond$` (sekmeli stop'lar).
`narwhal-vim` motoruna basit "tab-stop mode" ekle. Snippet store'a
parametrik şema (`{stops: [...]}`).

### 11. History search & favorites  *(P1, 2 gün)*

`:history /pattern`, `:history pin <id>`, `:history per-conn`.
JSONL grow'u için yıllık rotate (yarım MB'tan büyükse arka planda compact).

### 12. Multi-result tabs from batch  *(P2, 2 gün)*

Bir batch'te N statement → N ayrı result-tab.
`narwhal-app::core::state::result.rs` + `tab.rs` zaten parçalı sahip,
eşleştirme logic'i eksik.

### 13. Sidebar genişletme  *(P2, 3 gün)*

Sequences, materialized views, types/enums, extensions, foreign tables,
procedures, triggers. Driver başına introspection query + UI tree dalı.

---

## Önceliklendirme özeti

| # | Özellik | Sürüm | Efor | Etki |
|---|---------|-------|------|------|
| 1 | `:goto` fuzzy navigator | 1.1 | 3–4g | ★★★★★ |
| 2 | Color + write guards | 1.1 | 1–2g | ★★★★★ |
| 3 | EXPLAIN visualizer | 1.1 | 3–5g | ★★★★ |
| 4 | `sql::guard` ortaklaştır | 1.1 | ½g | ★ (borç) |
| 5 | Row editor pending UI | 1.2 | 1h | ★★★★★ |
| 6 | FK navigation gd/gr | 1.2 | 3g | ★★★★ |
| 7 | Inline filter/sort | 1.2 | 3–4g | ★★★ |
| 8 | Schema diff + `:alter` | 1.2 | 1h | ★★★ |
| 9 | SQL linter | 1.3 | 4–5g | ★★★ |
| 10 | Live templates | 1.3 | 2–3g | ★★ |
| 11 | History search | 1.3 | 2g | ★★ |
| 12 | Multi-result tabs | 1.3 | 2g | ★★ |
| 13 | Sidebar genişletme | 1.3 | 3g | ★★ |

---

## Çapraz kesen iş — yapılmazsa yukarısı dağılır

- **`narwhal-sql`'i AST seviyesine çıkar.** Splitter + formatter var,
  AST yok. `sqlparser-rs` eklemek (deny.toml + lisans onayı) ya da
  minimal kendi parser'ı yazmak (#7 #8 #9 hepsi buna dayanıyor).
- **Driver introspection cache.** `describe_table` her seferinde DB'ye
  gidiyor; `:goto` ve FK nav için connection-scoped cache + invalidate
  (`:refresh`). `narwhal-pool` yan modulü mantıklı yer.
- **`unwrap()` envanteri.** `src/`'de ~399 unwrap/expect var. Çoğu
  static regex (LazyLock'a göç) veya test (cargo, sorun değil) ama
  hot path'te kalanları (`config/url.rs`, `pre_connect.rs`, `keymap.rs`)
  Result'a çevir. CONTRIBUTING'de "production unwrap yasak" yazıyor.
- **`narwhal-app::core` god-module.** `results_actions.rs` 953, 
  `pending_actions.rs` 722, `run_loop.rs` 693 satır. Row editor 
  (#5) bunlara dokunacak; öncesinde fonksiyon-bazlı modülarize etmek 
  kaçınılmaz olacak.

---

## Açık sorular

1. `sqlparser-rs` mı, ev yapımı parser mı? Eski + büyük bağımlılık,
   ama #7/8/9'u tek başına ayağa kaldırır. Karar v1.2 başında.
2. ClickHouse row editor: hiç desteklemeyelim mi, yoksa `ALTER … UPDATE`
   üretip uyarayım mı? Topluluğa sor.
3. Color-coding renkleri: hardcoded 6 mi, kullanıcı hex mi versin?
   Hex = terminal renk uyumsuzluğu derdi; ilk sürüm 6 named color.
4. `:goto` hız hedefi: 10k obje gerçekçi mi, 100k mı? PG'de büyük
   schema'lar nadir; 10k baseline yeter, ölç sonra karar ver.

---

## Yapmayacaklarımız (kasıtlı liste)

- Görsel ER diagram editörü — terminalde gerçek değer yok, fareye muhtaç.
- Drag-drop query builder — narwhal vim odaklı, kimliğe ters.
- Embedded LLM completion — MCP yönü zaten "AI sana bağlanıyor". 
  Editör içi tamamlama ayrı bir ürün; ilgilenen `narwhal-plugin` 
  ile Lua üzerinden ekleyebilir.
- Bağımsız GUI build — `egui`/`tauri` kapı açma; ürün kimliği = TUI.
- Cloud sync of connections — keyring/pgpass yeterli, sync = saldırı yüzeyi.
