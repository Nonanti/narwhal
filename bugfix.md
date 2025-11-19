# Narwhal — Bug-Fix Uygulama Planı

> **Amaç:** `bug.md`'deki 71 bulgunun (C1-C6, H1-H20, M1-M23, L1-L40) **çakışmadan**,
> **doğrulanabilir** biçimde, **bir veya birden fazla AI agent** tarafından
> uygulanması.
>
> Bu plan her dalga için: (a) hangi bulguları içerir, (b) hangi dosyalara
> dokunur, (c) hangi dalga ile çakışır, (d) test stratejisi, (e) agent'a
> verilebilecek **hazır prompt**, (f) tamamlanma kriteri.
>
> **Önkoşul okumalar (her agent için):**
> - `/home/nonantiy/Projects/narwhal/AGENTS.md` veya `~/.pi/agent/AGENTS.md`
> - `/home/nonantiy/Projects/narwhal/bug.md`
> - `/home/nonantiy/Projects/narwhal/Cargo.toml` (workspace lints + deps)
> - İlgili crate'in `src/lib.rs` ve `tests/`

---

## 0. Genel Kurallar (Tüm Dalgalara Uygulanır)

### 0.1 Branch / Commit Stratejisi

- Her dalga ayrı feature branch: `fix/wave-1-critical`, `fix/wave-2-security`, ...
- Her bulgu ayrı commit (conventional commits):
  - `fix(<crate>): C1 — MySQL date bind format roundtrip kaldırıldı`
  - `fix(<crate>): H7 — history JSONL secret redaction + 0600`
  - `refactor(<crate>): M14 — non_exhaustive on public enums/traits`
  - `test(<crate>): C3 regression — RETURNING detection on multibyte`
- Commit mesajı gövdesi: kısa açıklama + **bug ID** referansı (`Refs: C3, H1`).
- Squash YAPMA — her bulgu git blame'de izlenebilir kalmalı.

### 0.2 Doğrulama (Her Commit'ten Sonra)

```bash
cargo fmt --all
cargo clippy --all-targets --workspace -- -D warnings
cargo test --workspace --all-features
```

Üçü de temiz olmadan commit YOK. Driver entegrasyon testleri Docker
(`docker compose -f tests/docker-compose.yml up -d`) gerektiriyorsa çalıştırılmalı.

### 0.3 Yasak Değişiklikler

- Workspace `Cargo.toml`'daki bağımlılıkların **major version** bump'i tek
  commit'te yapılamaz; ayrı `chore(deps):` commit'i.
- Public API'de breaking change (trait metod imzası, enum varyant tipi)
  yalnız M14 dalgasında. Diğer dalgalarda `#[non_exhaustive]` ve default
  impl ile geriye dönük uyumlu kalınmalı.
- `unsafe` kod EKLEME (workspace `unsafe_code = forbid`).
- `unwrap()`/`expect()` production kodunda EKLEME (yalnız test'te OK).
- `println!` debug dışında EKLEME (`tracing` kullan).

### 0.4 Dokümantasyon

- Public API değişikliği olan her commit `CHANGELOG.md` `[Unreleased]`
  bölümüne madde ekler.
- Kullanıcıya görünür davranış değişikliği (TLS default, secret redaction)
  `README.md` ve `docs/` altında belgelenir.

### 0.5 Test Stratejisi

- **Regression first:** Her CRITICAL/HIGH bulgu için **önce başarısız test**
  yaz, sonra fix uygula. Test commit'i fix commit'inden önce gelmeli (TDD).
- **Snapshot/integration testleri** mevcutsa kırılmamalı; kırılıyorsa
  açıklama: davranış değişikliğinin gerekçesi `bug.md`'deki ID'ye işaret etmeli.
- **Property-based:** İlgili yerlerde `proptest` (Unicode handling, splitter)
  önerilir.

---

## 1. Dalga 1 — CRITICAL (Veri Bütünlüğü & Panik)

> **Hedef:** Üretimde veri bozan / panik fırlatan / TUI'yi donduran 6 bulgu.
> **Süre tahmini:** 2-3 gün, tek developer.
> **Branch:** `fix/wave-1-critical`

### 1.1 Bulgular

| ID | Başlık | Crate | Tahmini effort |
|----|--------|-------|----------------|
| C1 | MySQL Date/Time bind round-trip | `narwhal-driver-mysql` | 1.5 saat |
| C2 | ClickHouse `replace_question_marks` UTF-8 | `narwhal-driver-clickhouse` | 3 saat |
| C3 | DuckDB `has_returning_clause` panik | `narwhal-driver-duckdb` | 1 saat |
| C4 | Editor cursor unicode + `set_cursor` boundary | `narwhal-tui` | 2 saat |
| C5 | Schema refresh session mismatch | `narwhal-app` | 3 saat |
| C6 | Streaming render throttle ölü | `narwhal-app` | 2 saat |

### 1.2 Dokunulan Dosyalar

| Dosya | Bulgu |
|-------|-------|
| `crates/narwhal-driver-mysql/src/types.rs` | C1 |
| `crates/narwhal-driver-mysql/tests/binding.rs` (yeni) | C1 |
| `crates/narwhal-driver-clickhouse/src/lib.rs` (520-563) | C2 |
| `crates/narwhal-driver-clickhouse/tests/binding.rs` (yeni) | C2 |
| `crates/narwhal-driver-duckdb/src/lib.rs` (194-203) | C3 |
| `crates/narwhal-driver-duckdb/tests/returning.rs` (yeni) | C3 |
| `crates/narwhal-tui/src/widgets/editor.rs` (127-130, 610-657) | C4 |
| `crates/narwhal-tui/Cargo.toml` (eğer `unicode-width` workspace'te değilse — değil, var) | — |
| `crates/narwhal-app/src/core.rs` (4346, 3399-3525) | C5 |
| `crates/narwhal-app/src/run.rs` (`RunContext` struct'a `session_id`) | C5 |
| `crates/narwhal-app/src/session.rs` (`Session.id: Uuid` zaten var) | C5 |
| `crates/narwhal-app/src/app.rs` (84-103) | C6 |
| `crates/narwhal-app/src/core.rs` (4283-4295, throttle decision noktası) | C6 |
| `crates/narwhal-app/tests/schema_refresh.rs` | C5 (mevcut testlere ek) |
| `crates/narwhal-app/tests/streaming.rs` (yeni) | C6 |

### 1.3 Çakışma Matrisi (Wave 1 içi)

```
       C1   C2   C3   C4   C5   C6
C1     —    ·    ·    ·    ·    ·
C2     ·    —    ·    ·    ·    ·
C3     ·    ·    —    ·    ·    ·
C4     ·    ·    ·    —    ·    ·
C5     ·    ·    ·    ·    —    ⚠
C6     ·    ·    ·    ·    ⚠    —    ← her ikisi de core.rs/app.rs'e dokunuyor

⚠ = aynı dosyada uzak satırlar, ardışık uygula (önce C5, sonra C6).
```

**Paralel güvenli alt-gruplar:**
- Grup A (paralel safe): **C1, C2, C3, C4** — 4 ayrı crate, hiç çakışma yok.
- Grup B (sıralı): **C5 → C6** — `core.rs` ve `app.rs` ortak.

### 1.4 Bağımlılık (Diğer Dalgalarla)

- C5 → H11 (MetaUpdate kanalı) ile bağlı değil; ayrı.
- C6'nın `STREAM_RENDER_THROTTLE` sabiti M23'te `constants` modülüne taşınacak —
  ilk önce literal const, sonra M23 refactor'unda taşıma. Çakışma yok.

### 1.5 Tamamlanma Kriteri

- [ ] 6 commit (her bulgu için 1) + 6 test commit'i (TDD).
- [ ] `cargo test --workspace` yeşil.
- [ ] `cargo clippy --all-targets --workspace -- -D warnings` yeşil.
- [ ] Aşağıdaki manuel regresyonlar:
  - Türkçe identifier ile ClickHouse `SELECT * FROM "kullanıcılar" WHERE ad = ?`
    çalışır.
  - DuckDB SQL'de `-- rüya x\nSELECT 1` panik atmaz.
  - TUI editörde `ş`/`ğ` yazıldıktan sonra cursor doğru yerde, popup doğru
    konumda.
  - 1M satırlık ClickHouse stream'inde F4 cancel yetişir.

### 1.6 Agent Prompt (Wave 1)

```
GÖREV: Narwhal Wave 1 — CRITICAL bug fix dalgası.

ÖNCE OKU (sırasıyla):
1. /home/nonantiy/Projects/narwhal/AGENTS.md
2. /home/nonantiy/Projects/narwhal/bug.md (özellikle "CRITICAL" bölümü: C1-C6)
3. /home/nonantiy/Projects/narwhal/bugfix.md (özellikle "Dalga 1" bölümü)

YÖNTEM:
- TDD: her bug için önce başarısız test (commit), sonra fix (commit).
- Conventional commits: `fix(<crate>): C<id> — <kısa açıklama>`.
- Sıra: önce paralel grup (C1, C2, C3, C4 — bağımsız crate'ler), sonra C5,
  en son C6.

KABUL KRİTERLERİ:
- `cargo fmt --all` temiz.
- `cargo clippy --all-targets --workspace -- -D warnings` temiz.
- `cargo test --workspace --all-features` yeşil.
- bug.md'deki "Düzeltme" bölümleri rehber; ID'lerle uyumlu commit mesajı.
- Manuel regresyonlar (bugfix.md 1.5) belgele (PR açıklaması veya
  CHANGELOG [Unreleased]).

YASAK:
- Public API breaking change yok (bu dalgada).
- `unwrap()`/`expect()` production kodda yok.
- `unsafe` yok.
- Wave 2+'a ait dosyalara dokunma.

TAMAMLANDIĞINDA:
- bug.md'deki C1-C6 maddelerine ✅ işareti ekle.
- Branch `fix/wave-1-critical` üzerinde 12 commit (6 test + 6 fix).
```

---

## 2. Dalga 2 — Güvenlik (TLS, Injection, Secret Handling)

> **Hedef:** Production'da güvenlik garanti veren değişiklikler.
> **Süre tahmini:** 3-4 gün.
> **Branch:** `fix/wave-2-security`
> **Önkoşul:** Wave 1 merge edilmiş.

### 2.1 Bulgular

| ID | Başlık | Crate | Effort |
|----|--------|-------|--------|
| H1 | PG `Prefer` AcceptAny → chain verify | `narwhal-driver-postgres` | 3 saat |
| H2 | PG connection-string injection → Config builder | `narwhal-driver-postgres` | 4 saat |
| H3 | PG cancel handle TLS connector | `narwhal-driver-postgres` | 2 saat |
| H7 | History JSONL secret redaction + 0600 | `narwhal-history` | 3 saat |
| H8 | Keyring async wrapper | `narwhal-config` | 3 saat |
| H9 | URL parser sslmode → struct field | `narwhal-config` | 2 saat |
| H13 | Wizard password zeroize | `narwhal-app` | 4 saat |
| M1 | PG `verify-ca` custom verifier | `narwhal-driver-postgres` | 3 saat |
| M2 | PG/MySQL `Require` aynı semantiğe | `narwhal-driver-postgres`, `narwhal-driver-mysql` | 1 saat |
| M3 | `ssl_root_cert` + `Disable` reddi | `narwhal-config` | 1 saat |
| M4 | ClickHouse `escape_sql_string` backslash | `narwhal-driver-clickhouse` | 1 saat |

### 2.2 Dokunulan Dosyalar

| Dosya | Bulgular |
|-------|----------|
| `crates/narwhal-driver-postgres/src/lib.rs` | H2, H3 |
| `crates/narwhal-driver-postgres/src/tls.rs` | H1, M1, M2 |
| `crates/narwhal-driver-postgres/Cargo.toml` | — (zaten gerekenler var) |
| `crates/narwhal-driver-postgres/tests/tls.rs` (yeni veya genişlet) | H1, H3, M1 |
| `crates/narwhal-driver-mysql/src/lib.rs` | M2 |
| `crates/narwhal-driver-clickhouse/src/lib.rs` | M4 |
| `crates/narwhal-history/src/journal.rs` | H7 |
| `crates/narwhal-history/Cargo.toml` (regex dep) | H7 |
| `crates/narwhal-history/tests/redaction.rs` (yeni) | H7 |
| `crates/narwhal-config/src/credentials.rs` | H8 |
| `crates/narwhal-config/src/url.rs` | H9 |
| `crates/narwhal-config/src/settings.rs` | M3 |
| `crates/narwhal-config/Cargo.toml` (`async-trait` zaten var) | — |
| `crates/narwhal-config/tests/url.rs` | H9 (mevcut test güncellenir) |
| `crates/narwhal-config/tests/tls.rs` | M3 |
| `crates/narwhal-app/src/wizard.rs` | H13 |
| `crates/narwhal-app/src/core.rs` (commit_wizard) | H13 |
| `Cargo.toml` (workspace deps: `zeroize`, `secrecy`) | H13 |

### 2.3 Çakışma Matrisi

```
        H1   H2   H3   H7   H8   H9   H13  M1   M2   M3   M4
H1      —    ·    ·    ·    ·    ·    ·    ⚠    ⚠    ·    ·
H2      ·    —    ⚠    ·    ·    ·    ·    ·    ·    ·    ·     ← lib.rs yan yana
H3      ·    ⚠    —    ·    ·    ·    ·    ·    ·    ·    ·
H7      ·    ·    ·    —    ·    ·    ·    ·    ·    ·    ·
H8      ·    ·    ·    ·    —    ·    ⚠    ·    ·    ·    ·     ← H13 keyring çağırır
H9      ·    ·    ·    ·    ·    —    ·    ·    ·    ⚠    ·     ← M3 SslMode validate
H13     ·    ·    ·    ·    ⚠    ·    —    ·    ·    ·    ·
M1      ⚠    ·    ·    ·    ·    ·    ·    —    ⚠    ·    ·     ← tls.rs ortak
M2      ⚠    ·    ·    ·    ·    ·    ·    ⚠    —    ·    ·
M3      ·    ·    ·    ·    ·    ⚠    ·    ·    ·    —    ·
M4      ·    ·    ·    ·    ·    ·    ·    ·    ·    ·    —

⚠ = aynı dosya, ardışık uygula.
```

**Sıra (önerilen):**
1. H9 (config/url) — bağımsız.
2. M3 (config/settings) — H9 sonrası SslMode wiring net.
3. H8 (credentials async) — bağımsız.
4. H13 (wizard zeroize) — H8 async API'sini kullanır.
5. H1 → M1 → M2 (tls.rs sırayla) — PG TLS subsystem'i bütün olarak.
6. H2 → H3 (lib.rs sırayla) — Config builder, sonra cancel handle.
7. H7 (history) — bağımsız.
8. M4 (clickhouse escape) — bağımsız.

**Paralel grup (3 agent):**
- Agent A: H9 → M3 → H1 → M1 → M2 → H2 → H3 (PG + config TLS yolu).
- Agent B: H8 → H13 (secret handling).
- Agent C: H7, M4 (independent).

### 2.4 Bağımlılık (Diğer Dalgalarla)

- H8 async trait değişimi → call site'ları `narwhal-app` etkilenir. Wave 5'te
  başka çağrılar gelmemeli; H8 commit'i tüm çağıranları aynı anda günceller.
- H13 `Cargo.toml` workspace dep ekler (`zeroize`, `secrecy`); diğer
  dalgalarla çakışmaz (yeni dep).
- H1/H2/H3 ve M14 (non_exhaustive) çakışmaz — M14 v1.0 SemVer Wave 5'te.

### 2.5 Davranış Değişikliği — Geriye Dönük Etki

| Bulgu | Etki |
|-------|------|
| H1 | **Breaking (security):** Mevcut `Prefer`/`Require` ile bağlanan self-signed PG sunucuları artık reddedilir. README'de migration notu: `?sslmode=disable` veya CA güveni. |
| H7 | History dosyası mevcutsa eski cleartext kayıtlar dokunulmaz; yeni kayıtlar redacted. Doc'a: "eski history dosyasını silin veya manuel redact edin". |
| H8 | Async trait breaking; tüm impl'ler güncellenir (mevcut tek impl `KeyringStore` + test mock). |
| H9 | `?sslmode=...` artık `options`'a düşmüyor; daha önce options'tan okuyan kod yok (driver'lar `ssl_mode` field'ından okuyor) — kullanıcıya görünür değil. |

### 2.6 Tamamlanma Kriteri

- [ ] 11 fix + 11 test commit (TDD).
- [ ] `cargo test --workspace --all-features` yeşil.
- [ ] Manuel testler:
  - Self-signed PG'ye `?sslmode=verify-full` ile bağlanma reddedilir.
  - `CREATE USER x PASSWORD 'secret'` history'de `'***'` olur.
  - Wizard'a girilen parola çekirdek dump'ında bulunmaz (manuel: gcore + grep).
- [ ] CHANGELOG'a "Security" bölümü.
- [ ] README'ye "TLS defaults changed" notu.

### 2.7 Agent Prompt (Wave 2)

```
GÖREV: Narwhal Wave 2 — Security dalgası.

ÖNCE OKU:
1. /home/nonantiy/Projects/narwhal/AGENTS.md
2. /home/nonantiy/Projects/narwhal/bug.md (H1-H3, H7-H9, H13, M1-M4)
3. /home/nonantiy/Projects/narwhal/bugfix.md (Dalga 2)
4. /home/nonantiy/Projects/narwhal/crates/narwhal-driver-postgres/src/tls.rs
5. /home/nonantiy/Projects/narwhal/crates/narwhal-config/src/url.rs
6. /home/nonantiy/Projects/narwhal/crates/narwhal-history/src/journal.rs

YÖNTEM:
- TDD: her bug için başarısız test → fix.
- bugfix.md 2.3 "Sıra" bölümündeki ardışıklığı takip et.
- Eğer 3 agent paralel çalışıyorsa 2.3 "Paralel grup" şemasını kullan.

ÖZEL UYARI — H1:
- `Prefer` ve `Require` davranışı **breaking change**. README'ye migration
  notu eklemeden commit etme.
- Geriye dönük escape hatch için yeni `SslMode::RequireInsecure` varyantı
  ekleme; doc'a `?sslmode=disable` öner.

ÖZEL UYARI — H7:
- regex'i `Lazy::new` ile statik olarak derle.
- Mode 0o600 yalnız Unix'te; Windows ACL'i `tokio::fs::OpenOptions` default
  bırak ve `#[cfg(unix)]` ile koşula al.
- Eski (cleartext) history dosyaları otomatik redact EDİLMEZ — sadece
  yeni yazımlar. Doc'a not ekle.

ÖZEL UYARI — H13:
- `Drop` impl panic etmemeli (zeroize panic-safe).
- `SecretString::expose_secret()` sadece keyring set sırasında çağrıl;
  ek String kopyası bırakma.

KABUL KRİTERLERİ:
- `cargo fmt --all`, `cargo clippy --all-targets --workspace -- -D warnings`,
  `cargo test --workspace --all-features` üçü temiz.
- CHANGELOG [Unreleased] "Security" maddeleri eklendi.
- README TLS bölümü güncel.

YASAK:
- Wave 1, Wave 3+ kapsamına dokunma.
- `unwrap()`/`expect()` production kodda.
```

---

## 3. Dalga 3 — MySQL Doğruluk

> **Hedef:** MySQL driver'da tip/stream/protocol kusurları.
> **Süre tahmini:** 2-3 gün.
> **Branch:** `fix/wave-3-mysql`
> **Önkoşul:** Wave 1 merge (C1 fix'i temel).

### 3.1 Bulgular

| ID | Başlık | Effort |
|----|--------|--------|
| H4 | Paramsız sorgu text protocol → binary | 4 saat |
| H5 | `stream()` gerçek stream veya capability flag | 3 saat |
| H6 | `Value::Timestamp` bind formatı | 1 saat |
| H10 | Splitter MySQL backslash escape | 3 saat |
| M10 | UNIQUE constraint `len()>1` filtresi | 0.5 saat |
| L29 | BLOB UTF-8 fallback | 1 saat |
| L30 | View tipi `describe_table` | 1 saat |

### 3.2 Dokunulan Dosyalar

| Dosya | Bulgular |
|-------|----------|
| `crates/narwhal-driver-mysql/src/lib.rs` | H4, H5, M10, L30 |
| `crates/narwhal-driver-mysql/src/types.rs` | H6, L29 |
| `crates/narwhal-driver-mysql/tests/*.rs` (genişlet) | H4-H6, M10, L29, L30 |
| `crates/narwhal-sql/src/splitter.rs` | H10 |
| `crates/narwhal-sql/tests/splitter.rs` | H10 |

### 3.3 Çakışma Matrisi

```
        H4   H5   H6   H10  M10  L29  L30
H4      —    ·    ·    ·    ·    ·    ·
H5      ·    —    ·    ·    ·    ·    ·
H6      ·    ·    —    ·    ·    ⚠    ·
H10     ·    ·    ·    —    ·    ·    ·    ← farklı crate
M10     ·    ·    ·    ·    —    ·    ·
L29     ·    ·    ⚠    ·    ·    —    ·
L30     ·    ·    ·    ·    ·    ·    —

⚠ = types.rs aynı dosya.
```

**Sıra (önerilen):**
1. H10 — bağımsız (narwhal-sql).
2. H6 → L29 (types.rs sırayla).
3. H4 → M10 → L30 (lib.rs sırayla).
4. H5 — bağımsız (lib.rs ama ayrı fonksiyon).

### 3.4 Bağımlılık

- H5 capability flag yolu → `narwhal-core` `Capabilities` struct'ına alan
  ekler. `#[non_exhaustive]` zaten var → backward-compat. M14'ten önce de OK.
- H4 binary protocol fallback için **integration test** Docker MySQL gerekir.

### 3.5 Tamamlanma Kriteri

- [ ] 7 fix + 7 test commit.
- [ ] Integration testleri Docker MySQL ile yeşil:
  ```bash
  cd crates/narwhal-driver-mysql
  docker compose -f tests/docker-compose.yml up -d
  cargo test --features integration
  docker compose -f tests/docker-compose.yml down
  ```
- [ ] `SELECT 1` paramsız → `Value::Int(1)` (string değil).
- [ ] `INSERT VALUES (?)` `Value::Timestamp(now())` ile MySQL kabul ediyor.
- [ ] MySQL splitter `'\';'` literal'i tek ifadeye sayar.

### 3.6 Agent Prompt (Wave 3)

```
GÖREV: Narwhal Wave 3 — MySQL doğruluk.

ÖNCE OKU:
1. /home/nonantiy/Projects/narwhal/AGENTS.md
2. /home/nonantiy/Projects/narwhal/bug.md (H4-H6, H10, M10, L29, L30)
3. /home/nonantiy/Projects/narwhal/bugfix.md (Dalga 3)
4. /home/nonantiy/Projects/narwhal/crates/narwhal-driver-mysql/src/{lib,types}.rs
5. /home/nonantiy/Projects/narwhal/crates/narwhal-sql/src/splitter.rs

YÖNTEM:
- TDD.
- Sıra: H10 (sql) → H6 + L29 (types) → H4 + M10 + L30 (lib) → H5 (lib).

ÖZEL UYARI — H4:
- Whitelist admin SQL'leri (`SAVEPOINT`, `SET TRANSACTION`, `USE`,
  `START TRANSACTION`, `BEGIN`, `COMMIT`, `ROLLBACK`, `RELEASE SAVEPOINT`,
  `ROLLBACK TO SAVEPOINT`). Bu pattern'lerde `query_iter` kalır.
- Diğer tüm paramsız sorgular `exec_iter(sql, Params::Empty)` kullansın.
- Regression: `SELECT 1`, `SELECT 1.5`, `SELECT 'text'`, `SELECT DATE '2024-01-02'`
  doğru `Value` döndürüyor.

ÖZEL UYARI — H5:
- Eğer gerçek streaming (mysql_async `stream_and_drop`) implementasyonu
  zor görünüyorsa **capability flag** yoluna git: `Capabilities` struct'ına
  `pub streaming: bool` ekle (default true), MySQL false döndür.
- App katmanı bunu görsel olarak yansıtsın: F7 stream tuşu MySQL için
  "buffered (not true streaming)" mesajı versin.

KABUL KRİTERLERİ:
- Tüm test'ler yeşil.
- bug.md'deki H4-H6, H10, M10, L29, L30 ✅ işaretli.
```

---

## 4. Dalga 4 — Performans & UI Hijyeni

> **Hedef:** UI donmaları, N+1, ClickHouse temizlik, TUI multibyte.
> **Süre tahmini:** 4-5 gün.
> **Branch:** `fix/wave-4-perf-ui`
> **Önkoşul:** Wave 1 (C5, C6) merge.

### 4.1 Bulgular

| ID | Başlık | Crate | Effort |
|----|--------|-------|--------|
| H11 | `MetaUpdate` kanalı (block_in_place kaldırma) | `narwhal-app` | 8 saat |
| H12 | `refresh_schemas` N+1 → `list_all_tables` | `narwhal-core`, drivers, `narwhal-app` | 6 saat |
| H15 | Lua timeout hook Mutex kaldırma | `narwhal-plugin-lua` | 2 saat |
| H16 | TUI multibyte highlight + history | `narwhal-tui` | 3 saat |
| H17 | Completion popup rect tutarlı | `narwhal-tui` | 2 saat |
| H19 | Pool unwrap/expect kaldırma | `narwhal-pool` | 2 saat |
| H20 | Plugin timeout deterministik plugin adı | `narwhal-app` | 1 saat |
| M5 | ClickHouse Float(NaN/Inf) | `narwhal-driver-clickhouse` | 0.5 saat |
| M6 | ClickHouse cancel idempotent | `narwhal-driver-clickhouse` | 1 saat |
| M7 | ClickHouse stream query_id leak (RAII guard) | `narwhal-driver-clickhouse` | 2 saat |
| M8 | PG extract_csv unit separator | `narwhal-driver-postgres` | 1 saat |
| M9 | PG prepared statement cache | `narwhal-driver-postgres` | 3 saat |
| M11 | View kind on describe_table (3 driver) | `narwhal-driver-sqlite`, `-duckdb`, `-clickhouse` | 2 saat |
| M12 | DuckDB Date32/Timestamp doğru render | `narwhal-driver-duckdb` | 3 saat |
| M13 | `Journal::recent` reverse + spawn_blocking | `narwhal-history` | 3 saat |
| M15 | Mouse preview cell-edit fix | `narwhal-app` | 1 saat |
| M16 | Vim operatör state machine | `narwhal-vim` | 4 saat |
| M17 | `pending_count` overflow guard | `narwhal-vim` | 0.5 saat |
| M18 | Plugin `_timeout_budget` registry'ye | `narwhal-plugin-lua` | 1 saat |
| M19 | Plugin name deterministik hash | `narwhal-plugin-lua` | 1 saat |
| M20 | TUI sanitize_for_grid (BIDI/control) | `narwhal-tui` | 3 saat |
| M21 | Status bar width unicode | `narwhal-tui` | 1 saat |

### 4.2 Dokunulan Dosyalar (Çakışma açısından kritik)

| Dosya | Bulgular |
|-------|----------|
| `crates/narwhal-app/src/core.rs` | H11, H12, H20, M15 |
| `crates/narwhal-app/src/app.rs` | H11 (MetaUpdate select arm) |
| `crates/narwhal-app/src/session.rs` | H12 |
| `crates/narwhal-app/src/meta.rs` (yeni) | H11 |
| `crates/narwhal-core/src/connection.rs` | H12 (`list_all_tables` default impl) |
| Tüm `crates/narwhal-driver-*/src/lib.rs` | H12 (M11, M12 ek olarak) |
| `crates/narwhal-driver-clickhouse/src/{lib,types}.rs` | M5, M6, M7 |
| `crates/narwhal-driver-postgres/src/lib.rs` | M8, M9 |
| `crates/narwhal-driver-duckdb/src/types.rs` | M12 |
| `crates/narwhal-tui/src/widgets/{editor,history,results}.rs` | H16, H17, M20, M21 |
| `crates/narwhal-tui/src/layout.rs` | H17, M21 |
| `crates/narwhal-plugin-lua/src/lib.rs` | H15, M18, M19 |
| `crates/narwhal-pool/src/pool.rs` | H19 |
| `crates/narwhal-history/src/journal.rs` | M13 |
| `crates/narwhal-vim/src/{machine,action,mode}.rs` | M16, M17 |
| `Cargo.toml` (workspace deps: `parking_lot`, `lru`, `rev_lines`) | H19, M7, M9, M13 |

### 4.3 Çakışma Matrisi (Yüksek Riskli)

```
core.rs ortak:    H11 ⚠ H12 ⚠ H20 ⚠ M15      → SIRALI: H20 → M15 → H12 → H11
clickhouse lib:   M5 ⚠ M6 ⚠ M7              → SIRALI: M5 → M6 → M7
pg lib:           M8 ⚠ M9                    → SIRALI: M8 → M9
tui editor:       H16 ⚠ M20                 → SIRALI: H16 → M20
tui layout:       H17 ⚠ M21                 → SIRALI: M21 → H17
plugin-lua lib:   H15 ⚠ M18 ⚠ M19           → SIRALI: H15 → M18 → M19
driver lib (her): H12 ⚠ M11 ⚠ M12 (duckdb)  → SIRALI per driver: M11 → M12 → H12
```

### 4.4 Paralel Çalıştırma — 4 Agent

Bu dalga büyük; 4 agent paralel verimli:

- **Agent A — App perf:** H20 → M15 → C5/C6 üstünde (yapıldıysa) → H12 (driver
  imzaları için H12 driver işi B'de) → H11 (MetaUpdate). H12 ve H11 sıralı.
- **Agent B — Drivers:** ClickHouse (M5→M6→M7), PG (M8→M9), DuckDB (M11→M12),
  SQLite (M11), her driver için H12 `list_all_tables` impl'i (Agent A
  trait imzasını commit ettikten sonra).
- **Agent C — TUI:** M21 → H17, H16 → M20. Bağımsızdır A/B'den.
- **Agent D — Misc:** H15 → M18 → M19 (plugin-lua), H19 (pool), M13
  (history), M16 → M17 (vim).

**Senkronizasyon noktası:** H12 trait imza commit'i (Agent A) önce gelmeli;
sonra Agent B impl'leri ekler.

### 4.5 Tamamlanma Kriteri

- [ ] 22 fix + en az 15 test commit'i.
- [ ] **UI yanıt süresi:** `dump_schema all` (50 tablo, 50ms latency mock)
  sırasında F4 key event 100ms içinde işlenir.
- [ ] **Stream throttle:** 100k satır stream'i CPU %50 altında.
- [ ] **Cancel idempotent:** ClickHouse'a 5 kez ardarda Ctrl-C basılınca
  her seferinde KILL QUERY denenir.
- [ ] **Vim operatörler:** `dw`, `yy`, `c$` çalışır.
- [ ] **Multibyte:** Türkçe SQL'i highlight'lamak panik atmaz.
- [ ] **BIDI:** Tablo değerinde `\u{202E}` `·` olarak görünür.

### 4.6 Agent Prompt (Wave 4)

```
GÖREV: Narwhal Wave 4 — Perf & UI hijyen dalgası.

ÖNCE OKU:
1. /home/nonantiy/Projects/narwhal/AGENTS.md
2. /home/nonantiy/Projects/narwhal/bug.md (H11-H12, H15-H17, H19-H20, M5-M21)
3. /home/nonantiy/Projects/narwhal/bugfix.md (Dalga 4)

BU AGENT'IN ROLÜ: [Agent A | B | C | D — bugfix.md 4.4'ten seç]

PARALEL ÇALIŞMA UYARILARI:
- H12 trait imza commit'i Agent A'da; Agent B impl başlamadan önce o
  commit'i pull etmeli.
- core.rs, lib.rs ve tui dosyaları paylaşılmıyor (her agent farklı dosya
  kümesi); ama branch tek (`fix/wave-4-perf-ui`) — git rebase her push
  öncesi şart.

YÖNTEM:
- TDD.
- 4.3 çakışma matrisindeki SIRALI işaretlemelere uy.

ÖZEL UYARI — H11:
- `MetaRequest`/`MetaUpdate` enum'larını `crates/narwhal-app/src/meta.rs`
  içinde tut.
- Mevcut `block_in_place` çağrılarını **bir bir** dönüştür; bir commit'te
  hepsini değiştirme — review imkansız olur.
- İlk PR: `dump_schema all` + `refresh_schemas` + `open_history` üçü.

ÖZEL UYARI — H12:
- `Connection::list_all_tables` default impl `Err(Error::Unsupported(...))`
  döndürsün; impl'i eklenmeyen driver mevcut N+1 yolunda kalır.
- App tarafında `if driver.supports_list_all_tables() { … } else { fallback N+1 }`.

ÖZEL UYARI — M16 (vim operatörler):
- Yeni Mode::OperatorPending(Operator) varyantı `Mode` enum'una eklenir;
  `#[non_exhaustive]` değil (zaten v1.0). M14 dalgasında eklenecek;
  o zamana kadar match exhaustive — tüm match'leri güncelle.

KABUL KRİTERLERİ:
- Tüm test'ler yeşil.
- 4.5 tamamlanma kriterleri manuel doğrulandı.
- bug.md'deki ilgili ID'ler ✅.
```

---

## 5. Dalga 5 — API Stability & Refactor

> **Hedef:** v1.x SemVer hapsi açma, modül bölünmesi, dead code temizliği.
> **Süre tahmini:** 3-4 gün.
> **Branch:** `fix/wave-5-api-cleanup`
> **Önkoşul:** Wave 1-4 merge.

### 5.1 Bulgular

| ID | Başlık | Effort |
|----|--------|--------|
| H18 | `EditorBuffer` SoC bölme | 6 saat |
| M14 | `#[non_exhaustive]` toplu ekleme | 2 saat |
| M22 | `ResultView.state` private + getter | 2 saat |
| M23 | TUI constants modülü | 2 saat |
| L1 | Display path alloc | 1 saat |
| L2 | ClickHouse Mutex → parking_lot | (Wave 4 M7 ile çakışır, sonra) | 0.5 saat |
| L3 | Splitter memmem | 1 saat |
| L4 | URL IPv6 | 2 saat |
| L5 | UUID benzersizlik | 1 saat |
| L6 | Pool max_size=0 assert | 0.5 saat |
| L7 | Boş query key | 0.5 saat |
| L8 | PG try_from int | 1 saat |
| L9 | DDL generated unwrap_or empty | 0.5 saat |
| L11 | SQLite/DuckDB path validate | 1 saat |
| L12 | Plugin doc fix | 0.5 saat |
| L13 | KeyMod/Mode Hash derive | 0.5 saat |
| L14 | Vim command_buffer cap | 0.5 saat |
| L15 | Editor ölü `.max` | 0.5 saat |
| L16 | move_word_forward newline | 1 saat |
| L17 | wrap_text grapheme | 1 saat |
| L18 | format_count boundary | 0.5 saat |
| L19 | format_elapsed boundary | 0.5 saat |
| L20 | main.rs `_settings` + sessiz hata | 0.5 saat |
| L21 | core.rs modül bölme | 8 saat |
| L22 | plugin_state poison handling | 0.5 saat |
| L23 | Tab field pub(crate) | 1 saat |
| L24 | Sidebar scroll | 3 saat |
| L25 | centred_rect DRY | 1 saat |
| L26 | tui re-export çiftlemesi | 0.5 saat |
| L27 | Pane::cycle_back | 0.5 saat |
| L28 | CH query_tsv stream eşiği | 2 saat |
| L31 | MySQL KILL QUERY | 4 saat |
| L32 | find_all dead max | 0.5 saat |
| L33 | İlk tab adı uyumu | 0.5 saat |
| L34 | parse_input tip tahmini | 1 saat |
| L35 | narwhal-tui tracing dep kaldır | 0.5 saat |
| L36 | GUTTER_WIDTH dinamik | 1 saat |
| L37 | ConfigPaths path-aware hata | 0.5 saat |
| L38 | HistoryEntry sql truncate | 1 saat |
| L39 | Pool idle_count poison | 0.5 saat |
| L40 | main.rs guard flush | 0.5 saat |

### 5.2 Dokunulan Dosyalar — Geniş

Neredeyse her dosya. Bu dalga **mekanik refactor** ağırlıklı; her commit
küçük.

### 5.3 Sıra

1. **Önce M14 (`#[non_exhaustive]`).** Tüm public enum/trait'lere ekle.
   Downstream match'ler derlenmeyi reddedebilir; her match'i güncelle
   (`_ => …`).
2. **H18 (EditorBuffer SoC).** Standalone PR; testler taşınır.
3. **L21 (core.rs bölme).** Standalone PR; mekanik refactor.
4. **M22, M23.** TUI API temizlik.
5. **L18, L19** boundary fix'leri (mevcut test'lere ek).
6. **L1-L17, L20-L40** — tek seferde çoklu commit (her ID ayrı commit).

### 5.4 Çakışma

Bu dalganın doğası mekanik; aynı dosyalara çok kez dokunur. Tek agent
ideal. Paralel çalışılacaksa:

- **Agent A:** M14 → H18 → L21 (yapısal).
- **Agent B:** Geri kalan L'ler (5'er-10'ar gruplar halinde).

### 5.5 Tamamlanma Kriteri

- [ ] Tüm public enum'lar `#[non_exhaustive]`.
- [ ] `narwhal-tui` artık `narwhal-sql` dependency'sine sahip değil.
- [ ] `core.rs` 4858 satırdan en fazla 1500 satıra düştü; geri kalanı
  `core/{results,tabs,run_loop,transactions,plugins}.rs`.
- [ ] `cargo clippy --all-targets --workspace -- -D warnings` temiz.
- [ ] `cargo doc --no-deps --workspace` warning-free.
- [ ] CHANGELOG `[Unreleased]` "Breaking", "Refactor", "Cleanup" sections.

### 5.6 Agent Prompt (Wave 5)

```
GÖREV: Narwhal Wave 5 — API stability & refactor.

ÖNCE OKU:
1. /home/nonantiy/Projects/narwhal/AGENTS.md
2. /home/nonantiy/Projects/narwhal/bug.md (M14, M22, M23, H18, tüm L'ler)
3. /home/nonantiy/Projects/narwhal/bugfix.md (Dalga 5)

YÖNTEM:
- Mekanik refactor; davranış değişmez.
- Sıra: bugfix.md 5.3.
- M14 öncesi: tüm public enum'ları rg ile bul:
  rg --type rust '^pub enum' crates/

ÖZEL UYARI — M14:
- Trait'lere yeni metod ekleme; sadece existing trait'lere `#[non_exhaustive]`
  attribute'u eklemek için trait'in default impl politikası yeterli.
- Downstream match'leri kırıyorsa `_ => Error::Unsupported(...)` veya
  `_ => unreachable!("v1.x add new variant")` ekle.

ÖZEL UYARI — H18:
- `EditorBuffer` saf metin/cursor kalır; statement split + auto-pair iş
  kuralları `narwhal-app::editor` modülüne (yeni dosya) taşınır.
- `narwhal-tui/Cargo.toml`'dan `narwhal-sql` dep'ini kaldır.
- Test'leri taşı; davranış parity için snapshot test'leri (varsa) tut.

ÖZEL UYARI — L21:
- `core.rs`'i bölmek için tek seferde tüm impl'leri taşıma; modül modül
  ilerle (önce results, sonra tabs, ...). Her modül ayrı commit.

KABUL KRİTERLERİ:
- Tüm test'ler yeşil.
- `cargo doc` warning-free.
- bug.md'deki ilgili ID'ler ✅.
```

---

## 6. Çapraz Dalga Bağımlılık Özeti

```
            ┌─── Wave 1 (Critical) ─────────────────────┐
            │   C1, C2, C3, C4, C5, C6                  │
            └──┬─────────────────────────────┬──────────┘
               │                             │
               ▼                             ▼
        Wave 2 (Security)            Wave 3 (MySQL)
        H1, H2, H3, H7, H8, H9,      H4, H5, H6, H10,
        H13, M1, M2, M3, M4          M10, L29, L30
               │                             │
               └──────────────┬──────────────┘
                              ▼
                  Wave 4 (Perf & UI)
                  H11, H12, H15, H16, H17, H19, H20,
                  M5-M13, M15-M21
                              │
                              ▼
                  Wave 5 (API & Refactor)
                  H18, M14, M22, M23, L1-L40
```

**Paralel pencereler:**
- Wave 2 ve Wave 3 paralel çalışabilir (farklı driver/subsystem).
- Wave 4 Wave 1'in C5/C6'sına bağımlı (run_loop dokunuyor).
- Wave 5 son.

---

## 7. Çakışma Önleme Cheatsheet — Hangi Dosya Hangi Dalgada?

| Dosya | W1 | W2 | W3 | W4 | W5 |
|-------|----|----|----|----|----|
| `narwhal-core/connection.rs` | — | — | — | H12 | M14 |
| `narwhal-core/value.rs` | — | — | — | — | M14, L1 |
| `narwhal-config/credentials.rs` | — | H8 | — | — | — |
| `narwhal-config/url.rs` | — | H9 | — | — | L4, L7 |
| `narwhal-config/settings.rs` | — | M3 | — | — | L5 |
| `narwhal-pool/pool.rs` | — | — | — | H19 | L6, L39 |
| `narwhal-history/journal.rs` | — | H7 | — | M13 | L38, M14 |
| `narwhal-sql/splitter.rs` | — | — | H10 | — | L3, M14 |
| `narwhal-driver-postgres/lib.rs` | — | H2, H3 | — | M8, M9, H12 | — |
| `narwhal-driver-postgres/tls.rs` | — | H1, M1, M2 | — | — | — |
| `narwhal-driver-postgres/types.rs` | — | — | — | — | L8 |
| `narwhal-driver-postgres/ddl.rs` | — | — | — | — | L9 |
| `narwhal-driver-mysql/lib.rs` | — | M2 | H4, M10, L30, H5 | H12 | L31 |
| `narwhal-driver-mysql/types.rs` | C1 | — | H6, L29 | — | — |
| `narwhal-driver-sqlite/lib.rs` | — | — | — | M11, H12 | L11 |
| `narwhal-driver-duckdb/lib.rs` | C3 | — | — | M11, H12 | L11 |
| `narwhal-driver-duckdb/types.rs` | — | — | — | M12 | — |
| `narwhal-driver-clickhouse/lib.rs` | C2 | M4 | — | M5, M6, M7, M11, H12 | L2, L28 |
| `narwhal-plugin/lib.rs` | — | — | — | — | — |
| `narwhal-plugin-lua/lib.rs` | — | — | — | H15, M18, M19 | — |
| `narwhal-vim/machine.rs` | — | — | — | M16, M17 | L14 |
| `narwhal-vim/mode.rs` | — | — | — | M16 | L13 |
| `narwhal-vim/action.rs` | — | — | — | M16 | — |
| `narwhal-vim/key.rs` | — | — | — | — | L13 |
| `narwhal-tui/widgets/editor.rs` | C4 | — | — | H16, M20 | H18, L15, L16, L36 |
| `narwhal-tui/widgets/history.rs` | — | — | — | H16, M20 | — |
| `narwhal-tui/widgets/results.rs` | — | — | — | M20, M22 | L18, L19 |
| `narwhal-tui/widgets/row_detail.rs` | — | — | — | M20 | L17, L25 |
| `narwhal-tui/widgets/sidebar.rs` | — | — | — | M20 | L24 |
| `narwhal-tui/widgets/help.rs` | — | — | — | — | L25 |
| `narwhal-tui/widgets/wizard.rs` | — | — | — | — | L25 |
| `narwhal-tui/widgets/snippets.rs` | — | — | — | — | — |
| `narwhal-tui/layout.rs` | — | — | — | H17, M21 | M23, L27 |
| `narwhal-tui/lib.rs` | — | — | — | — | L26, L35 |
| `narwhal-app/core.rs` | C5, C6 | H13 | — | H11, H12, H20, M15 | L21, L22, L23, L32, L33 |
| `narwhal-app/app.rs` | C6 | — | — | H11 | — |
| `narwhal-app/session.rs` | C5 | — | — | H12 | — |
| `narwhal-app/run.rs` | C5 | — | — | — | — |
| `narwhal-app/wizard.rs` | — | H13 | — | — | — |
| `narwhal-app/edit.rs` | — | — | — | — | L34 |
| `narwhal-app/meta.rs` (yeni) | — | — | — | H11 | — |
| `narwhal/src/main.rs` | — | — | — | — | L20, L40 |
| `Cargo.toml` (workspace) | — | H13 (zeroize/secrecy) | — | H19 (parking_lot), M9 (lru), M13 (rev_lines) | — |

> **Kural:** İki AI agent **aynı dosyaya** **aynı dalga içinde** ardı ardına
> dokunmamalı. Yukarıdaki tabloda aynı dosyada birden fazla bulgu varsa
> bugfix.md ilgili dalganın "Sıra" bölümünde belirlenen sıra ile **tek agent**
> uygulanmalı, ya da farklı commit'lere bölünüp birbiri ardına rebase
> edilmeli.

---

## 8. Agent Koordinasyon Protokolü (Çok Agentli Çalışma)

### 8.1 Branch Per Agent (Önerilen)

Her agent kendi sub-branch'ı: `fix/wave-4-perf-ui-agent-a`.
Wave tamamlanınca PR'lar `fix/wave-4-perf-ui`'ye merge, oradan main'e
single PR.

### 8.2 Senkronizasyon Noktaları

Wave 4'te kritik:
- H12 trait imza commit'i Agent A'dan gelir → diğer agent'lar `git pull --rebase`.
- M14 (`#[non_exhaustive]`) öncesi tüm Wave 4 işleri biter.

### 8.3 Çakışma Halinde

- Git merge conflict ÇIKARSA: agent durur, durum raporu yazar, koordinatör
  (insan veya orchestrator agent) çözüm verir.
- Manuel conflict resolution: bug.md ID'lerine göre — daha düşük ID
  önceliklidir (eg. H1 > M5 conflict çıkarsa H1'in değişikliği baz alınır).

### 8.4 İletişim

Her agent commit etmeden önce **`progress.md`** (gitignore'da) günlüğüne
yazsın:

```markdown
## 2026-05-22 14:00 — Agent A — Wave 4
- ✅ H20 fix + test
- 🔄 M15 in progress
- ⏳ H12 trait imza commit'i bekliyorum (Agent B'ye sinyal)
```

Diğer agent'lar bu dosyayı okur, çakışmayı önler.

---

## 9. Toplam Effort & Takvim Tahmini

| Dalga | Toplam saat | Tek dev | 2 dev paralel | 4 dev paralel |
|-------|-------------|---------|---------------|----------------|
| Wave 1 | 12.5 | 2 gün | 1.5 gün | 1 gün |
| Wave 2 | 30 | 4 gün | 2.5 gün | 1.5 gün |
| Wave 3 | 13.5 | 2 gün | 1.5 gün | 1 gün |
| Wave 4 | 56 | 7 gün | 4 gün | 2.5 gün |
| Wave 5 | 60 | 8 gün | 5 gün | 3 gün |
| **Toplam** | **172** | **23 gün** | **14.5 gün** | **9 gün** |

> AI agent hızı insan dev'e yakın ama doğrulama (clippy/test/manuel) sabit
> sürer; bu nedenle çok-agent'lı kazanım maksimum **2-3×**.

---

## 10. Risk Matrisi

| Risk | Olasılık | Etki | Azaltma |
|------|----------|------|----------|
| H1 (TLS default) kullanıcı bağlantısını koparır | Yüksek | Yüksek | README migration notu + `?sslmode=disable` escape hatch |
| H11 (MetaUpdate) regresyon | Orta | Yüksek | Aşamalı çevirim (yol başına 1 PR) + integration test |
| H18 (EditorBuffer SoC) snapshot test'leri kırar | Orta | Orta | Davranış parity testleri taşı, snapshot diff'leri review |
| M14 (`#[non_exhaustive]`) downstream consumer'ları kırar | Düşük (henüz publish yok) | Düşük | İlk publish öncesi yapılıyor |
| Çoklu agent merge conflict | Yüksek | Orta | Dosya bazlı slice, progress.md koordinasyon |
| Driver integration testleri Docker gerektirir | Kesin | Düşük | CI matrix Docker ile çalışır (mevcut) |

---

## 11. CHANGELOG Şablonu

Her dalga sonunda `CHANGELOG.md` `[Unreleased]` bölümüne ekle:

```markdown
## [Unreleased]

### Security

- **TLS defaults strengthened** (H1, M1, M2): `Prefer` and `Require` no
  longer accept invalid certificates. Migration: explicit `?sslmode=disable`
  for self-signed setups or trust the CA.
- **History redacts secrets** (H7): `CREATE USER … PASSWORD '…'` patterns
  are masked in `~/.local/share/narwhal/history.jsonl`. Existing entries
  unaffected.
- **Connection string injection closed** (H2): Postgres driver now uses
  `tokio_postgres::Config` builder with options whitelist.
- **Wizard password zeroized** (H13).

### Fixed

- **MySQL Date/Time roundtrip** (C1): years outside u16 no longer silently
  become `0000-00-00`.
- **ClickHouse non-ASCII identifiers** (C2): parameterised queries on
  `"kullanıcılar"`-style tables now work.
- **DuckDB RETURNING detection no longer panics** on multibyte SQL (C3).
- **Editor cursor on Turkish/CJK input** (C4).
- **Schema refresh after DDL targets the originating session** (C5).
- **Streaming render throttle re-engaged** (C6).
- ... (her ID için bir madde)

### Changed

- `Connection`, `DatabaseDriver`, `Value`, `Outcome`, `Dialect`, `SslMode`,
  `IsolationLevel`, `TableKind` artık `#[non_exhaustive]` (M14).
- `narwhal-tui` artık `narwhal-sql` bağımlılığı içermiyor (H18).
- `CredentialStore` trait async (H8); migration: `#[async_trait]` ekleyin.

### Performance

- Postgres schema fetch: 50+ ardışık RTT yerine tek sorgu (H12).
- UI tıkanmaları: `MetaUpdate` kanalı ile `describe_table` /
  `dump_schema all` non-blocking (H11).
- Lua plugin timeout hook'u mutex-free (H15).

### Refactor

- `narwhal-app/core.rs` 4858 satırdan 1500 satıra düştü, modüllere
  bölündü (L21).
```

---

## 12. SON KONTROL — Ana Repo Sahibi (Berkant) İçin

Her dalga merge öncesi:

1. [ ] Tüm bulgu ID'leri ✅ — `rg "C[0-9]+" bug.md | rg -v ✅` boş.
2. [ ] CI yeşil (Docker, MacOS, Linux matrix).
3. [ ] CHANGELOG güncel.
4. [ ] README user-facing değişiklik notları güncel.
5. [ ] `cargo publish --dry-run` her publishable crate için temiz.
6. [ ] Manuel duman testi:
   - `narwhal` aç, demo PG bağlan, `:open`, `:refresh`, sorgu çalıştır.
   - ClickHouse'a Türkçe identifier'lı sorgu.
   - MySQL `SELECT 1` → grid'de "1" tipi int.
   - F4 cancel yetişir.
   - Wizard'da parola gir, `:forget` çalışır.
   - History açıldı, secret yok.

Tamamlandığında: tag `v1.1.0` (eğer breaking H1, M14 varsa) veya
`v1.0.1` (yalnız fix'ler).
