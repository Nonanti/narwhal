# Narwhal — Bug Registry

> Kaynak: 6 paralel reviewer raporu (`/tmp/narwhal-review/*.md`), 2026-05-21.
> Tüm bulgular ID'li, severity'ye göre sıralı. Her madde *bağımsız* uygulanabilecek
> şekilde yazıldı — başka bir AI agent'a tek tek verilebilir.

## Şiddet Skalası

- **CRITICAL** — Veri bozma, panik (worker düşürür), güvenlik regresyonu, kullanıcı erişilemez kalır.
- **HIGH** — Production blocker; doğru davranışı sessizce ihlal, güvenlik defense-in-depth.
- **MEDIUM** — Davranış tutarsızlığı, performans kaybı, UX bozulması, sertleştirme.
- **LOW** — Temizlik, dead code, doc, minör cilalama.

## ID Sözlüğü

- `C##` = Critical, `H##` = High, `M##` = Medium, `L##` = Low.
- Numaralandırma sıkıdır; yeni bulgu eklenirse listeye eklenir, mevcut ID'ler değişmez.

---

# CRITICAL

## C1 ✅ — MySQL Date/Time bind: `format().parse()` round-trip, tarihleri sessizce `0000-00-00` yapıyor

- **Dosya:** `crates/narwhal-driver-mysql/src/types.rs:15-44`
- **Tetik:** Herhangi bir `Value::Date` / `Value::Time` / `Value::DateTime` parametresi.
- **Etki:**
  1. `MyValue::Date` yılı `u16`; `chrono::NaiveDate` yılı `i32` (negatif olabilir).
     `v.format("%Y").to_string().parse().unwrap_or(0)` — yıl `-0001` veya `12345`
     ise `u16` parse başarısız → `0` → MySQL tarihi `0000-00-00` veya reddediliyor.
  2. Her bind 6× string allocation + 6× parse. Bulk insert'te ölçülebilir maliyet.
  3. Ay/gün/saat aralık dışı ise sessiz veri kaybı (`unwrap_or(0)`).
- **Mevcut kod:**
  ```rust
  Value::Date(v) => MyValue::Date(
      v.format("%Y").to_string().parse().unwrap_or(0),
      v.format("%m").to_string().parse().unwrap_or(0),
      ...
  )
  ```
- **Düzeltme:**
  ```rust
  use chrono::{Datelike, Timelike};

  Value::Date(v) => {
      let year = u16::try_from(v.year())
          .map_err(|_| MyError::Other(format!("year out of range: {}", v.year())))?;
      MyValue::Date(year, v.month() as u8, v.day() as u8, 0, 0, 0, 0)
  }
  Value::Time(v) => MyValue::Time(
      false, 0,
      v.hour() as u8, v.minute() as u8, v.second() as u8,
      v.nanosecond() / 1_000,
  ),
  Value::DateTime(v) => {
      let year = u16::try_from(v.year()).map_err(...)?;
      MyValue::Date(
          year, v.month() as u8, v.day() as u8,
          v.hour() as u8, v.minute() as u8, v.second() as u8,
          v.nanosecond() / 1_000,
      )
  }
  ```
- **Test:** `crates/narwhal-driver-mysql/tests/` altına;
  - `bind_date_preserves_year_month_day` (2024-01-02 round-trip)
  - `bind_date_rejects_year_out_of_range` (NaiveDate'ten i32 negatif yıl)
  - `bind_datetime_roundtrip_microsecond` (123_456_789 ns → 123_456 µs)

---

## C2 ✅ — ClickHouse `replace_question_marks`: UTF-8 dizisi Latin-1 olarak bozuluyor

- **Dosya:** `crates/narwhal-driver-clickhouse/src/lib.rs:520-563`
- **Tetik:** `params` boş değil **ve** SQL non-ASCII içeriyor (Türkçe identifier,
  string literal, yorum).
- **Etki:** Parametreli `SELECT * FROM "kullanıcılar" WHERE ad = ?` çağrısında
  `kullanıcılar` → `kullanÄ±cÄ±lar`; ClickHouse "Unknown table" döner. Test
  fixture'larında non-ASCII yok, CI yakalamıyor.
- **Mevcut kod:**
  ```rust
  let bytes = sql.as_bytes();
  for i in 0..bytes.len() {
      let c = bytes[i];
      // ...
      result.push(c as char);   // u8 → char = U+0000..U+00FF
  }
  ```
- **Düzeltme:** `char_indices()` üzerinden gez; string-içi/dışı durum ASCII
  karakterlere bağlı (`'`, `"`, `\\`), char bazında izlenebilir; ASCII olmayan
  karakterleri olduğu gibi `push` et.
  ```rust
  let mut out = String::with_capacity(sql.len());
  let mut state = State::Normal;
  let mut placeholder_idx = 0usize;
  for (_, ch) in sql.char_indices() {
      match state {
          State::Normal => match ch {
              '\'' => { state = State::SingleQuote; out.push(ch); }
              '"'  => { state = State::DoubleQuote; out.push(ch); }
              '?'  => {
                  let lit = render_literal(&params[placeholder_idx])?;
                  out.push_str(&lit);
                  placeholder_idx += 1;
              }
              other => out.push(other),
          },
          State::SingleQuote => { /* ... */ }
          // ...
      }
  }
  ```
- **Yan sorun:** `sql.contains('$')` heuristic'i `'$1.99'` literal'ini yanlış yola
  sokar; aynı düzeltmede `$N` yolu yalnız gerçek placeholder ise tetiklenmeli.
- **Test:**
  - `replace_question_marks_preserves_non_ascii_identifier`
  - `replace_question_marks_preserves_non_ascii_in_string_literal`
  - `replace_question_marks_does_not_misfire_on_dollar_in_literal`

---

## C3 ✅ — DuckDB `has_returning_clause` `&str` slice panik

- **Dosya:** `crates/narwhal-driver-duckdb/src/lib.rs:194-203`
- **Tetik:** Non-ASCII SQL; `r/R` byte'ından sonraki 9. pozisyon multibyte char'ın
  ortasına denk geldiğinde **`&sql[i..i+9]` panic** atar (`assertion failed:
  self.is_char_boundary(idx)`). Panik `spawn_blocking` içinde olduğundan task
  düşer, üst katmana hata değil **kapanan kanal** döner — kullanıcı sebebi
  anlamaz.
- **Örnek SQL:**
  ```sql
  -- rüya x  (ü = 0xC3 0xBC, 2 byte)
  SELECT 1;
  ```
- **Mevcut kod:**
  ```rust
  if (c == b'R' || c == b'r')
      && i + 9 <= bytes.len()
      && sql[i..i + 9].eq_ignore_ascii_case("RETURNING")
  ```
- **Düzeltme:** Byte slice ve `[u8]::eq_ignore_ascii_case` kullan:
  ```rust
  if (c == b'R' || c == b'r')
      && bytes.len() - i >= 9
      && bytes[i..i + 9].eq_ignore_ascii_case(b"RETURNING")
      && is_word_boundary(bytes, i, i + 9)
  ```
- **`is_word_boundary` helper:** önceki byte ASCII identifier-char değil (veya
  pozisyon 0), sonraki byte ASCII identifier-char değil (veya pozisyon =
  `bytes.len()`).
- **Test:**
  - `returning_detection_does_not_panic_on_multibyte`
  - `returning_detection_handles_word_boundary` (`customer_returning` false-pos vermesin)

---

## C4 ✅ — Editor cursor unicode-uyumsuz + `EditorBuffer::set_cursor` char-boundary doğrulamıyor

- **Dosya:** `crates/narwhal-tui/src/widgets/editor.rs:127-130, 610-614, 649-657`
- **Tetik:**
  1. **Görsel hata:** `cursor_x = (GUTTER_WIDTH + buffer.cursor_col) as u16` —
     `cursor_col` bayt; ekran sütununa eşitlenmiş. Türkçe karakter veya emoji
     girilen her satırda cursor yanlış konumda, autocomplete popup yanlış
     yerde.
  2. **Panik:** `set_cursor` çağıran herhangi bir kod (mouse click,
     `apply_motion` çıktısı, snippet expansion) `col` parametresinin
     char-boundary olduğunu garantilemiyor. Sonraki `insert_char` /
     `delete_char` / `insert_str('\n')` panik (`split_off`, `replace_range`,
     `current_line_mut().insert(col, ch)`).
- **Düzeltme — ekran genişliği:**
  ```rust
  use unicode_width::UnicodeWidthStr;
  let line = &buffer.lines[buffer.cursor_row];
  let prefix_end = buffer.cursor_col.min(line.len());
  let display_col = line[..prefix_end].width();
  let cursor_x = (GUTTER_WIDTH + display_col) as u16;
  ```
- **Düzeltme — boundary:**
  ```rust
  fn set_cursor(&mut self, row: usize, col: usize) {
      self.cursor_row = row.min(self.lines.len().saturating_sub(1));
      let line = &self.lines[self.cursor_row];
      let mut col = col.min(line.len());
      while col > 0 && !line.is_char_boundary(col) { col -= 1; }
      self.cursor_col = col;
  }
  ```
- **Bağlı:** `editor_cursor_anchor` ve `render_completion_popup` aynı düzeltmeyi alır.
- **Test:**
  - `cursor_x_handles_turkish_chars` (`şahin` yazıp cursor end'e gidince x=5)
  - `set_cursor_snaps_back_to_char_boundary` (multibyte ortasına set → snap)
  - `insert_after_motion_no_panic` (snippet bir `ü`'nün ardına cursor koysun → `insert_char('x')` no panic)

---

## C5 ✅ — Schema refresh yanlış oturumu tazeler (oturum değişimi sırasında)

- **Dosya:** `crates/narwhal-app/src/core.rs:4346, 3505-3525, 3399-3470`
- **Senaryo:**
  1. Session A'da `CREATE TABLE …` çalıştır.
  2. Sonuçlar gelmeden sidebar'dan B'yi aç.
  3. `RunUpdate::AllDone { ddl: true }` gelir → `schedule_schema_refresh()` 200ms
     debounce → `refresh_schema()` `self.session.as_mut()` üzerinden **B**'nin
     şemasını tazeler. A'nın DDL'i sidebar'da görünmez.
- **Etki:** Sidebar inconsistency, B üzerinde gereksiz RTT'ler, kullanıcı kafası karışır.
- **Düzeltme — iki yaklaşımdan biri:**
  - **(a) Guard:** `open_connection`, `close_session`, `remove_connection` çağrılarını
    `self.running || self.run_tab.is_some()` kontrolüne sok; çalışırken oturum
    değişimi reddedilsin (status mesajı: "query running, cancel first").
  - **(b) Session-bound refresh:** `RunContext`'e `session_id: Uuid` ekle;
    `SchemaRefresh` arm'ı yalnızca o id hâlâ aktifse `refresh_schema()` çalıştırsın.
- **Tercih:** (b) — kullanıcıya friction yaratmaz, race-free.
- **Test:**
  - `schema_refresh_skipped_when_session_changed` (mock: A çalışırken B aç → refresh A'nın schemasını tazelemeli, B değil)

---

## C6 ✅ — Streaming render throttle ölü kod (UI 1M rows/sn'de kilitlenir)

- **Dosya:** `crates/narwhal-app/src/core.rs:4283-4295`, `crates/narwhal-app/src/app.rs:84-103`
- **Mevcut:** `ResultState::Running.last_render` 100ms throttle vaat ediyor,
  `RowsAppended` arm'ı `last_render`'ı güncelliyor. Ama `App::run` `select!`
  her iterasyonunda **koşulsuz** `self.draw(&mut guard)?` çağırıyor;
  `last_render` hiçbir karar yolunda okunmuyor.
- **Etki:** STREAM_BATCH=64'te saniyede ~15 redraw zararsız, ama 1M rows/sn
  üreten bir stream'de event loop render'a kilitlenir, F4 cancel yetişmez.
- **Düzeltme:** `app.rs` event loop'unda redraw'u gate'le:
  ```rust
  let mut last_draw = Instant::now();
  loop {
      tokio::select! {
          // ... existing arms
      }
      let now = Instant::now();
      let force = self.core.is_modal_active() || self.core.dirty_takes_priority();
      if force || now.duration_since(last_draw) >= STREAM_RENDER_THROTTLE {
          self.draw(&mut guard)?;
          last_draw = now;
      }
  }
  ```
- **Ek temizlik:** `ResultState::Running.last_render` artık gereksiz (decision app
  loop'unda); kaldırılabilir veya değişiklik tespit metriği olarak bırakılır.
- **Sabit:** `const STREAM_RENDER_THROTTLE: Duration = Duration::from_millis(100);`
- **Test:**
  - `app_redraw_throttled_during_stream` (mock stream 1000 RowsAppended → draw çağrısı ≤ 11)
  - `app_redraw_immediate_for_key_event` (key event geldikten sonra throttle bekleme yok)

---

# HIGH / MAJOR

## H1 ✅ — Postgres `SslMode::Prefer` (default!) sertifika doğrulamasız TLS'e dönüşüyor

- **Dosya:** `crates/narwhal-driver-postgres/src/tls.rs:62-67`
- **Mevcut:**
  ```rust
  SslMode::Prefer | SslMode::Require => InternalSslMode::Require,
  ```
  → `Require` `insecure_client_config` (AcceptAny verifier) kullanıyor.
- **Etki:** Hiçbir TLS alanı set edilmediğinde varsayılan davranış MITM'e açık.
  `lib.rs:3-12` doc-comment'i "default: no TLS (disable)" diyor, **yalan**.
- **Düzeltme — tercih (a):**
  - `Prefer` → önce sistem-CA verifier ile TLS dene, başarısızsa plain'e düş
    (libpq semantiği).
  - `Require` → AcceptAny YOK; sistem-CA verifier (chain doğrula, hostname doğrula).
  - `VerifyCa` → chain doğrula, hostname atla (custom verifier — bkz. M1).
  - `VerifyFull` → full verify (mevcut).
- **Düzeltme — tercih (b, daha az iş):**
  - `Prefer` → sistem-CA verifier (handshake başarısızsa plain'e düşme bonus).
  - `Require` → AcceptAny yerine sistem-CA verifier; gerçekten AcceptAny isteyen
    kullanıcı yeni `SslMode::RequireInsecure` veya `?sslmode=require-insecure`
    eklesin.
- **Doc:** `lib.rs:3-12` ve `crates/narwhal-core/src/connection.rs:20-27`
  güncelle.
- **Test:**
  - `prefer_uses_chain_verifier` (mock TLS server self-signed → connection reject)
  - `verify_full_rejects_hostname_mismatch`
  - `disable_does_not_negotiate_tls`

---

## H2 ✅ — Postgres connection-string injection (şifre/options libpq escape edilmemiş)

- **Dosya:** `crates/narwhal-driver-postgres/src/lib.rs:135-149`
- **Mevcut:**
  ```rust
  let mut out = format!("host={host} port={port} dbname={database} user={user}");
  if let Some(pw) = password { out.push_str(&format!(" password={pw}")); }
  for (k, v) in &config.params.options { out.push_str(&format!(" {k}={v}")); }
  ```
- **Etki:**
  1. Boşluk/`'`/`\` içeren şifreler libpq tarafından yanlış parse edilir veya
     sonraki keyword'u bozar.
  2. `options.x = "y password=evil"` ile keyword enjekte edilir → sslmode
     override, user override.
  3. Bozuk değer parse hatası "connection failed" olarak yüzeye çıkar, root
     cause görünmez.
- **Düzeltme:** `tokio_postgres::Config` builder'a geç:
  ```rust
  let mut cfg = tokio_postgres::Config::new();
  cfg.host(host)
     .port(port)
     .user(user)
     .dbname(database);
  if let Some(pw) = password { cfg.password(pw); }

  const OPTIONS_WHITELIST: &[&str] =
      &["application_name", "connect_timeout", "options"];
  for (k, v) in &config.params.options {
      if !OPTIONS_WHITELIST.contains(&k.as_str()) {
          return Err(Error::Config(format!("unsupported option: {k}")));
      }
      match k.as_str() {
          "application_name" => { cfg.application_name(v); }
          "connect_timeout"  => { cfg.connect_timeout(parse_duration(v)?); }
          "options"          => { cfg.options(v); }
          _ => unreachable!(),
      }
  }
  let connector = make_tls_connector(&config.params)?;
  let (client, conn) = cfg.connect(connector).await?;
  ```
- **Yan kazanım:** Config builder password'u Debug formatında otomatik gizler.
- **Test:**
  - `password_with_space_roundtrips` (entegrasyon test ile mock PG)
  - `options_keyword_injection_rejected`
  - `unknown_option_returns_config_error`

---

## H3 ✅ — Postgres cancel handle her zaman `NoTls` (TLS bağlantılarda iptal kırık)

- **Dosya:** `crates/narwhal-driver-postgres/src/lib.rs:730-740`
- **Mevcut:**
  ```rust
  self.token.cancel_query::<NoTls>(NoTls).await
  ```
- **Etki:** Postgres iptali yeni TCP bağlantısı açar. Sunucu sadece TLS
  dinliyorsa cancel reddedilir. `capabilities.cancellation = true` reklamı
  TLS modunda yalan oluyor.
- **Düzeltme:** Kullanılan TLS modunu `PostgresConnection`'da sakla, cancel
  handle aynı connector ile iptal etsin:
  ```rust
  struct PostgresConnection {
      ...
      tls_factory: Arc<dyn Fn() -> MakeRustlsConnect + Send + Sync>,
  }

  impl CancelHandle for PostgresCancelHandle {
      async fn cancel(&self) -> Result<()> {
          let connector = (self.tls_factory)();
          self.token.cancel_query(connector).await
              .map_err(|e| Error::Connection(e.to_string()))
      }
  }
  ```
- **Test:**
  - `cancel_works_on_tls_server` (integration, TLS PG, uzun sorgu + cancel)

---

## H4 ✅ — MySQL paramsız sorgu text protocol → INT kolonlar `Value::String`

- **Dosya:** `crates/narwhal-driver-mysql/src/lib.rs:264-284`
- **Mevcut:**
  ```rust
  if bound.is_empty() {
      conn.query_iter(sql.as_str()).await?   // text protocol
  } else {
      conn.exec_iter(sql.as_str(), Params::from(bound)).await?  // binary
  }
  ```
- **Etki:** Text protocol tüm değerleri `MyValue::Bytes` döndürür;
  `value_from_my` `String` denerse UTF-8 başarılı olur → `SELECT 1` →
  `Value::String("1")`. TUI grid'i `"1000000"`'u `"999"`'dan önce sıralar
  (lexicographic). Aggregation, JSON export, cell edit bozuk.
- **Düzeltme — iki yol:**
  - **(a)** `bound.is_empty()` durumunda da `conn.exec_iter(sql, Params::Empty)` kullan.
    Ön koşul: `SAVEPOINT x`, `SET TRANSACTION`, `USE db`, `START TRANSACTION` gibi
    binary-incompatible admin statement'ları whitelist'le ve onlar için text protocol kalsın.
  - **(b)** `collect_text` içinde `column.column_type()` ve `flags` (UNSIGNED, BINARY)
    bilgisini kullanarak text bytes'ı doğru `Value`'ya parse et (int/float/decimal/date).
- **Tercih:** (a) — daha az kod, kolon tip eşlemesi binary protocol'de zaten doğru.
- **Test:**
  - `select_one_returns_int_not_string`
  - `select_float_returns_float`
  - `savepoint_uses_text_protocol_and_succeeds` (whitelist regression)

---

## H5 ✅ — MySQL `stream()` aslında full buffer (kontratı yalan söylüyor)

- **Dosya:** `crates/narwhal-driver-mysql/src/lib.rs:288-300`
- **Mevcut:**
  ```rust
  async fn stream(&mut self, ...) -> Result<Box<dyn RowStream>> {
      let materialised = self.execute(sql, params).await?;
      Ok(Box::new(BufferedRowStream { ... }))
  }
  ```
- **Etki:** Büyük tabloda OOM. `Connection::stream` doc'u (`narwhal-core`)
  "Streams release server-side resources only when …" diyor; MySQL kontratı
  yarı yarıya yalanlıyor.
- **Düzeltme yolları:**
  1. **Gerçek stream:** `mysql_async::QueryResult::stream_and_drop` ile
     bağlantıyı stream'e devret; `Drop`'ta `Arc<Mutex<Option<Conn>>>`'a geri
     koy. Karmaşık ama doğru.
  2. **Capability işareti:** `Capabilities` trait'ine `pub streaming: bool`
     ekle (zaten `#[non_exhaustive]`), MySQL false döndür; üst katman gerçek
     stream isteyince hatayı görsel olarak verir.
- **Tercih:** (2) önce ship et; (1) v1.1 feature olarak ayrı plan.
- **Test:**
  - `stream_buffers_for_now_but_advertises_correctly` (capability flag check)
  - (1) seçilirse: `stream_releases_rows_as_they_arrive`

---

## H6 ✅ — MySQL `Value::Timestamp` bind formatı kabul edilmiyor (C1 ile birlikte düzeltildi)

- **Dosya:** `crates/narwhal-driver-mysql/src/types.rs:46-49`
- **Mevcut:**
  ```rust
  Value::Timestamp(v) => MyValue::Bytes(v.to_rfc3339().into_bytes()),
  ```
- **Etki:** MySQL DATETIME literal formatı `YYYY-MM-DD HH:MM:SS[.ffffff]`;
  RFC3339 `T` ayracı ve timezone offseti içerir → MySQL reddediyor.
- **Düzeltme:**
  ```rust
  Value::Timestamp(v) => {
      let naive = v.naive_utc();
      let year = u16::try_from(naive.year())
          .map_err(|_| MyError::Other(format!("year out of range: {}", naive.year())))?;
      MyValue::Date(
          year, naive.month() as u8, naive.day() as u8,
          naive.hour() as u8, naive.minute() as u8, naive.second() as u8,
          naive.nanosecond() / 1_000,
      )
  }
  Value::Uuid(v) => MyValue::Bytes(v.hyphenated().to_string().into_bytes()),
  Value::Json(v) => MyValue::Bytes(serde_json::to_vec(v)?),
  ```
- **Not:** Bağlantı timezone'u UTC dışıysa convert davranışı sürpriz olabilir;
  doc'a "Timestamp UTC'ye normalize edilip naive olarak gönderilir" notu ekle.
- **Test:** `bind_timestamp_inserts_correctly`, `bind_uuid_inserts_correctly`.

---

## H7 ✅ — History JSONL secret leak + dosya izni umask'a tabi

- **Dosya:** `crates/narwhal-history/src/journal.rs:32-46, 142-151`
- **Etki:**
  - `HistoryEntry.sql` cleartext yazılıyor. Sızabilecek örnekler:
    - `CREATE USER x WITH PASSWORD 'plaintext'`
    - `ALTER USER x WITH PASSWORD '...'`
    - `COPY ... CREDENTIALS '...'`
    - `SET PASSWORD = '...'` (MySQL)
  - Dosya `tokio::fs::OpenOptions` ile açılıyor, mode set yok → umask'a tabi
    (çoğu sistemde `0644`); başka kullanıcı okuyabilir.
- **Düzeltme:**
  1. `Journal::open` Unix'te `OpenOptionsExt::mode(0o600)` ile aç.
  2. Yazmadan önce redactor:
     ```rust
     static REDACT_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| vec![
         Regex::new(r"(?i)password\s+'[^']*'").unwrap(),
         Regex::new(r"(?i)identified\s+by\s+'[^']*'").unwrap(),
         Regex::new(r"(?i)credentials\s+'[^']*'").unwrap(),
         Regex::new(r"(?i)set\s+password\s*=\s*'[^']*'").unwrap(),
     ]);

     fn redact(sql: &str) -> Cow<str> {
         let mut s = Cow::Borrowed(sql);
         for re in REDACT_PATTERNS.iter() {
             if re.is_match(&s) {
                 s = Cow::Owned(re.replace_all(&s, "$0'***'").to_string());
                 // veya tamamen yeniden yaz
             }
         }
         s
     }
     ```
  3. `Settings`'e `history.redact_secrets: bool = true` ekle, opt-out edilebilir
     ama varsayılan açık.
- **Test:**
  - `history_file_mode_is_0600`
  - `history_redacts_create_user_password`
  - `history_redacts_alter_user_password`
  - `history_does_not_redact_arbitrary_string`

---

## H8 ✅ — Keyring çağrıları async runtime'da bloklayıcı

- **Dosya:** `crates/narwhal-config/src/credentials.rs:38-72`
- **Etki:** `CredentialStore` trait sync; `keyring 3.x` Secret Service / DBus
  üzerinden bloklayıcı I/O yapıyor. `pool.acquire()`, `open_named` async
  path'lerinden çağrıldığında tokio worker saniyelerce stall (DBus down,
  gnome-keyring locked).
- **Düzeltme — iki yol:**
  - **(a)** `CredentialStore` async trait:
    ```rust
    #[async_trait]
    pub trait CredentialStore: Send + Sync {
        async fn get(&self, id: Uuid) -> Result<Option<SecretString>>;
        async fn set(&self, id: Uuid, secret: &SecretString) -> Result<()>;
        async fn forget(&self, id: Uuid) -> Result<()>;
    }
    ```
    İmpl `tokio::task::spawn_blocking` ile sarsın.
  - **(b)** Sync trait kalsın, çağıran taraf `spawn_blocking` ile sarsın.
- **Tercih:** (a) — call site'lar zaten async.
- **Bağlantılı:** `H13` (wizard password zeroize) ile aynı dosyaya dokunur,
  planlamada sıralı yapılmalı.
- **Test:**
  - `keyring_get_does_not_block_runtime` (tokio current-thread'de hızlı işlem ölçümü zor; flaky olmayan unit test: `spawn_blocking` yolunun çağrıldığını mock'la doğrula)

---

## H9 ✅ — URL parser `?sslmode=require` yutuluyor

- **Dosya:** `crates/narwhal-config/src/url.rs:140-149`
- **Etki:** Kullanıcı `postgres://...?sslmode=require&sslrootcert=/x.pem` yazsa
  bile `ConnectionParams.ssl_mode` `Prefer` (default) kalır;
  `validate_connections` TLS validation'ı tetiklenmiyor → sürücüye göre
  güvenlik regresyonu (H1'in default'una düşer).
- **Düzeltme:** `parse_query` çıktısında özel anahtarları çıkar:
  ```rust
  let mut options = BTreeMap::new();
  for (k, v) in parse_query(query)? {
      match k.as_str() {
          "sslmode" => params.ssl_mode = SslMode::parse(&v)?,
          "sslrootcert" => params.ssl_root_cert = Some(PathBuf::from(v)),
          "sslcert" => params.ssl_cert = Some(PathBuf::from(v)),
          "sslkey" => params.ssl_key = Some(PathBuf::from(v)),
          _ => { options.insert(k, v); }
      }
  }
  params.options = options;
  ```
- **`SslMode::parse`:** "disable"|"prefer"|"require"|"verify-ca"|"verify-full"
  tanır, diğerlerinde `UrlError::InvalidSslMode(value)`.
- **Test:**
  - `url_parses_sslmode_to_struct_field`
  - `url_parses_sslrootcert_path`
  - `url_rejects_unknown_sslmode`
  - `existing_test_url_passes_sslmode_into_options` — bu davranış değişiyor; mevcut test güncellenmeli (`url.rs:228-249`).

---

## H10 ✅ — MySQL splitter `\'` backslash escape tanımıyor

- **Dosya:** `crates/narwhal-sql/src/splitter.rs:216-225`
- **Etki:**
  - MySQL'de default `NO_BACKSLASH_ESCAPES` kapalı. `SELECT '\\';' ; SELECT 2`
    splitter tarafından yanlış yerden bölünür.
  - PostgreSQL `E'\n'` string'leri için aynı sorun (E prefix tanınmıyor).
- **Düzeltme:**
  ```rust
  enum StringMode { Standard, BackslashEscape }

  impl Splitter {
      fn string_mode(&self) -> StringMode {
          match self.dialect {
              Dialect::MySql => StringMode::BackslashEscape,
              Dialect::Postgres if self.in_e_prefix => StringMode::BackslashEscape,
              _ => StringMode::Standard,
          }
      }
  }
  ```
  `State::StringLiteral` BackslashEscape modunda `\\` görünce bir sonraki
  karakteri yutsun (tırnak kapatma değil).
  PG `E'...'` için: önceki token `E`/`e` ise `in_e_prefix = true`.
- **Test:**
  - `mysql_splits_correctly_with_backslash_escape`
  - `postgres_e_string_escape_sequence_does_not_close_literal`
  - `mysql_session_with_no_backslash_escapes_mode_still_correct` (opsiyonel)

---

## H11 ✅ — UI bloklayan `block_in_place + block_on` 17 noktada

- **Dosya:** `crates/narwhal-app/src/core.rs` (satırlar: 834, 2068, 2102, 2207, 2659, 3329, 3421, 3427, 3480, 3634, 3699, 3834, 4104, 4448)
- **Etki:** `describe_table`, `dump_schema all`, `refresh_schemas`, `open_history`,
  `commit_cell_edit` gibi yollarda UI 5-10sn donar; F4 dahi yetişmez. Worst case:
  `dump_schema all` 50 tabloda sıralı describe.
- **Düzeltme — yapısal:** `MetaUpdate` kanalı ekle (`RunRequest`/`RunUpdate` modeli
  gibi).
  ```rust
  enum MetaRequest {
      DescribeTable { tab: usize, schema: String, table: String },
      RefreshSchema { session_id: Uuid },
      DumpSchemaAll  { tab: usize },
      LoadHistory    { limit: usize },
  }
  enum MetaUpdate {
      DescribeTableReady { tab: usize, ddl: String },
      SchemaReady        { session_id: Uuid, tree: SchemaTree },
      HistoryReady       { entries: Vec<HistoryEntry> },
      MetaFailed         { msg: String },
  }
  ```
  `App::run` `select!`'ine ek arm, worker tokio task'a sahip.
- **Düzeltme — minimal (geçici):** Sadece `dump_schema all`, `refresh_schemas`,
  `open_history` `tokio::spawn` arkasına alınsın; sonuç bir `oneshot` ile UI'a
  dönsün. Modal modlu işlemler (`commit_cell_edit`) zaten kullanıcıyı bloklamak
  istiyor, dokunmasak da olur.
- **Tercih:** Aşamalı; ilk PR `MetaUpdate` skeleton + en yavaş 3 yolu taşır.
- **Test:**
  - `dump_schema_all_does_not_block_ui` (mock 50 tablo, 50ms latency her birinde;
    test 1sn'de geri dönmeli — yapısal olarak run loop'a key event push edebilmeli)

---

## H12 ✅ — `refresh_schemas` N+1 (her şema için ayrı `list_tables`)

- **Dosya:** `crates/narwhal-app/src/session.rs:71-101`
- **Etki:** 50 şemalı PG'de `:open` ve `:refresh` 50+ ardışık RTT.
- **Düzeltme:**
  1. **Driver-level:** `Connection` trait'ine `list_all_tables(&mut self) ->
     Result<Vec<(Schema, Vec<Table>)>>` ekle (default impl mevcut N+1 fallback);
     PG impl `information_schema.tables` tek sorguda döker:
     ```sql
     SELECT table_schema, table_name, table_type
     FROM information_schema.tables
     WHERE table_schema NOT IN ('pg_catalog','information_schema')
     ORDER BY table_schema, table_name;
     ```
  2. **App-level fallback:** Hâlâ N+1 ise `futures::future::try_join_all` ile
     paralel — ama `Connection` `&mut self` aldığı için pool'dan ayrı
     bağlantılar gerekir (`max_size >= concurrency`).
- **Tercih:** (1) — net hızlanma, paralelden temiz.
- **MySQL:** `information_schema.tables` aynı SQL.
- **SQLite:** zaten tek sorgu (`sqlite_master`).
- **DuckDB:** `duckdb_tables UNION ALL duckdb_views`.
- **ClickHouse:** `system.tables`.
- **Test:**
  - `list_all_tables_returns_all_schemas` (mock PG `pg_namespace`+`pg_class`)

---

## H13 ✅ — Wizard password belleği zeroize edilmiyor

- **Dosya:** `crates/narwhal-app/src/wizard.rs:177-285`, `core.rs:3879-3915`
- **Etki:** `WizardField.value: String` parola tutar; `build()` `to_owned()` ile
  kopyalar; `commit_wizard` `built.password.clone()` ile keyring'e set eder. En
  az 3 String kopyası bellekte zeroize edilmeden bırakılır. Process dump/core
  dump'ta cleartext.
- **Düzeltme:**
  ```rust
  // Cargo.toml
  zeroize = { version = "1", features = ["derive"] }
  secrecy = "0.10"

  // wizard.rs
  use secrecy::SecretString;
  use zeroize::Zeroize;

  enum WizardFieldValue {
      Public(String),
      Secret(SecretString),
  }
  ```
  Commit sırasında `secret.expose_secret()` ile **tek seferlik** kopya, keyring'e
  yaz, sonra `field.value.zeroize()`. `parse_url` `ParsedUrl.password:
  Option<SecretString>` olsun.
- **İlgili:** `H8` ile aynı subsystem; planda sıralı yap.
- **Test:**
  - `wizard_password_zeroized_after_commit` (alanı drop sonrası memcmp; zor — minimal: parola alanını `SecretString`'e yazıp `Debug` impl'inin sızdırmadığını doğrula)

---

## H14 ✅ — `outcome_from_lua` `return true` reject + `{sql=42}` sessiz yutma

- **Dosya:** `crates/narwhal-plugin-lua/src/lib.rs:391-422`
- **Etki:**
  1. Doc "returning nil or false is silent" diyor; `return true` `other` branch'e
     düşer, "unsupported return value: boolean" hatası. Tipik bir Lua "tamam,
     sessiz" dönüşü kırık.
  2. `{sql = 42}` — `t.get::<String>("sql")` Err döner; kod sessizce `status`
     kontrolüne düşer, en sonda "must have a 'sql' or 'status' field" der.
     Asıl tip hatası gizleniyor.
- **Düzeltme:**
  ```rust
  fn outcome_from_lua(v: LuaValue) -> Result<CommandOutcome, String> {
      match v {
          LuaValue::Nil => Ok(CommandOutcome::Silent),
          LuaValue::Boolean(_) => Ok(CommandOutcome::Silent),
          LuaValue::String(s) => Ok(CommandOutcome::Status(s.to_str()?.to_owned())),
          LuaValue::Table(t) => {
              if t.contains_key("sql")? {
                  let sql: String = t.get("sql")
                      .map_err(|_| "'sql' field must be a string".to_string())?;
                  let append = t.get::<Option<bool>>("append")?.unwrap_or(true);
                  Ok(CommandOutcome::Sql { sql, append })
              } else if t.contains_key("status")? {
                  let status: String = t.get("status")
                      .map_err(|_| "'status' field must be a string".to_string())?;
                  Ok(CommandOutcome::Status(status))
              } else {
                  Err("table must have a 'sql' or 'status' field".to_string())
              }
          }
          other => Err(format!("unsupported return value: {}", other.type_name())),
      }
  }
  ```
- **Doc güncelle:** `lib.rs:1-65` modül başlığında `nil | false | true` üçünün
  de silent olduğunu belirt.
- **Test:**
  - `return_true_is_silent`
  - `return_false_is_silent`
  - `table_with_sql_non_string_returns_typed_error`
  - `table_with_status_non_string_returns_typed_error`

---

## H15 ✅ — Timeout hook her Lua satırında Mutex lock

- **Dosya:** `crates/narwhal-plugin-lua/src/lib.rs:362-380`
- **Etki:** `HookTriggers::EVERY_LINE` her bytecode satırında
  `Arc<Mutex<InvocationTimeout>>` kilitleniyor. Tight transform loop'unda
  ölçülebilir overhead; struct mutate edilmiyor (yalnız `started_at`, `budget`
  okunuyor, `timed_out` zaten `AtomicBool`).
- **Düzeltme:**
  ```rust
  struct InvocationTimeout {
      started_at: Instant,
      budget: Duration,
      timed_out: AtomicBool,
  }
  // Arc<Mutex<...>> -> Arc<InvocationTimeout>

  lua.set_hook(HookTriggers::EVERY_LINE, move |_lua, _| {
      if timeout.started_at.elapsed() >= timeout.budget {
          timeout.timed_out.store(true, Ordering::Release);
          Err(mlua::Error::external("execution timeout exceeded"))
      } else {
          Ok(())
      }
  })?;
  ```
- **Yan kazanım:** `Send + Sync` doğrudan; Mutex poison sorunu yok.
- **Test:** Mevcut `tests/timeout.rs` regressionları geçmeli; ek mikrobenchmark
  istenirse `criterion` ile.

---

## H16 ✅ — Editor search highlight + history modal byte/char karışıklığı

- **Dosya:**
  - `crates/narwhal-tui/src/widgets/editor.rs:608-633` (highlight slicing)
  - `crates/narwhal-tui/src/widgets/history.rs:104-119, 137-147` (sql_width vs byte)
- **Etki:**
  - Search highlight `line_text[pos..col]` byte slice — boundary olmayan eşleşmede
    panik.
  - `truncate_str` `s.len() <= max` (bayt) ile karşılaştırıyor, ama `sql_width`
    display-cell sayısı; CJK/emoji içeren SQL'de modal taşar.
- **Düzeltme — highlight:**
  ```rust
  let line = &line_text;
  for hl in highlights.iter().filter(|h| h.row == row) {
      let start = floor_char_boundary(line, hl.start);
      let end   = floor_char_boundary(line, hl.end.min(line.len()));
      if start >= end { continue; }
      spans.push(Span::styled(&line[start..end], highlight_style));
  }
  ```
  `floor_char_boundary` Rust stable'da yok; manuel:
  ```rust
  fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
      idx = idx.min(s.len());
      while idx > 0 && !s.is_char_boundary(idx) { idx -= 1; }
      idx
  }
  ```
- **Düzeltme — history modal:**
  ```rust
  use unicode_width::UnicodeWidthStr;

  fn truncate_display(s: &str, max_width: usize) -> String {
      if s.width() <= max_width { return s.to_owned(); }
      let mut out = String::new();
      let mut w = 0;
      for ch in s.chars() {
          let cw = ch.to_string().width();
          if w + cw + 1 > max_width { out.push('…'); break; }
          out.push(ch);
          w += cw;
      }
      out
  }
  ```
  `format!("{ts:width$}", ts=ts, width=ts_width)` yerine manuel pad:
  ```rust
  fn pad_to_width(s: &str, w: usize) -> String {
      let mut out = s.to_owned();
      let need = w.saturating_sub(s.width());
      out.extend(std::iter::repeat(' ').take(need));
      out
  }
  ```
- **Test:**
  - `search_highlight_handles_multibyte_match`
  - `history_truncate_respects_display_width`
  - `history_pad_handles_wide_chars`

---

## H17 ✅ — `LayoutRegions::completion` rect tutarsız (mouse hit-test yanlış)

- **Dosya:** `crates/narwhal-tui/src/layout.rs:138-153`,
  `crates/narwhal-tui/src/widgets/editor.rs:710-716`
- **Etki:** `render_root` popup'ı **daima yukarı** olarak hesaplıyor;
  `render_completion_popup` yer varsa **aşağı** yerleştiriyor → mouse hit-test
  yanlış konumda. Yorum "approximate; not used for hit-testing" diyor ama field
  public, çağıran kontrolsüz.
- **Düzeltme:** `render_completion_popup` gerçek `Rect`'i döndürsün;
  `LayoutRegions`'a o değer atansın:
  ```rust
  pub struct CompletionHitRegions {
      pub popup: Rect,
      pub items: Vec<(Rect, usize)>,  // (rect, item_index)
  }
  // LayoutRegions.completion: Option<CompletionHitRegions>
  ```
  `render_root` `render_completion_popup`'tan dönen tuple'ı al ve doldur.
- **Test:**
  - `completion_popup_below_anchor_when_room`
  - `completion_popup_above_anchor_when_no_room`
  - `mouse_click_on_popup_item_dispatches_correct_index`

---

## H18 ✅ — `EditorBuffer` SoC ihlali (`narwhal-sql` bağımlılığı tui'de)

- **Dosya:** `crates/narwhal-tui/src/widgets/editor.rs:73-460`
- **Etki:**
  - Saf metin/cursor + Vim-motion + auto-pair + SQL-aware statement split tek
    struct'ta.
  - `narwhal-sql::Dialect` bağımlılığı tui crate'in build grafiğinde.
  - `narwhal-tui`'yi alternatif backend (Helix/GPUI) ile yeniden kullanmak imkansız.
- **Düzeltme — aşamalı:**
  1. `statement_at_cursor`/`all_statements` → `narwhal-app::editor` modülüne taşı,
     `EditorBuffer` referansı alsın.
  2. Auto-pair iş kuralları → `trait InsertPolicy { fn should_auto_pair(...) -> bool }`;
     default impl `narwhal-app`'te.
  3. `narwhal-sql` dependency'sini `narwhal-tui/Cargo.toml`'dan kaldır.
- **Dikkat:** Görsel davranış değişmemeli; testler yeni modüle taşınmalı.
- **Bağımlılık:** Bu refactor tek başına büyük; H16/H17 ile çakışmaz ama
  paralel yapılırsa merge conflict yüksek (editor.rs aynı dosya).
- **Test:** Mevcut editor testleri (`narwhal-tui` içinde) `narwhal-app::editor`
  altında çalışmalı; davranış parity.

---

## H19 ✅ — Pool `unwrap`/`expect` ihlalleri (workspace yasaklamış)

- **Dosya:** `crates/narwhal-pool/src/pool.rs:93, 135, 191, 199`
- **Etki:** 4 satırda invariant defensive panic. `AGENTS.md` `unwrap()/expect()`
  production kodda yasak; `clippy -- -D warnings` yine de geçiyor çünkü clippy
  bunu hata değil suggestion'a kaydetmiş.
- **Düzeltme:**
  1. `std::sync::Mutex<HashSet<...>>` → `parking_lot::Mutex` (poison yok).
     ```toml
     # workspace Cargo.toml
     parking_lot = "0.12"
     ```
  2. `PooledConnection` `Option<Box<dyn Connection>>` yerine
     `ManuallyDrop<Box<dyn Connection>>` kullan; `Drop`'ta `ManuallyDrop::take`.
     Deref'ler doğrudan `&*self.connection` / `&mut *self.connection` olur,
     `expect` gerekmez.
- **Test:** `pool_capacity_is_bounded` zaten var; ekle:
  - `pool_drop_runs_close_on_inner` (mock Connection drop counter)
  - `pool_idle_count_does_not_panic_under_load`

---

## H20 ✅ — Plugin command timeout `plugin_for(command)` deterministik değil

- **Dosya:** `crates/narwhal-app/src/core.rs:3340-3350`
- **Etki:** `Err(PluginError::Timeout)` yakalandığında plugin adı tekrar
  aranıyor (`plugin_for(command).map(|p| p.name())`); aynı head'i kaydetmiş
  iki plugin varsa yanlış ad raporlanabilir. Ayrıca timeout süresi
  ("timed out after 5.0s") actionable değil — config anahtarına referans yok.
- **Düzeltme:**
  ```rust
  let plugin_name = self.plugins.plugin_for(command).map(|p| p.name().to_owned());
  // ... block_on(dispatch)
  match outcome {
      Err(PluginError::Timeout) => {
          let label = plugin_name.as_deref().unwrap_or(command);
          self.status.set_error(format!(
              "plugin `{}` exceeded narwhal.execution_timeout_secs ({:.1}s); \
               adjust with `narwhal.set_timeout(secs)`",
              label, budget.as_secs_f64()
          ));
      }
      // ...
  }
  ```
- **Test:** `plugin_timeout_uses_resolved_plugin_name` (mock plugin yavaş
  handler, timeout 0.1s).

---

# MEDIUM

## M1 ✅ — Postgres `verify-ca` sessizce `verify-full`'a eşleniyor

- **Dosya:** `crates/narwhal-driver-postgres/src/tls.rs:7-13, 65`
- **Etki:** libpq `verify-ca` = "chain doğrula, hostname kontrolünü atla".
  rustls full validation yapıyor → hostname yanlış yapılandırılmış ama
  geçerli sertifika ile **bağlantı kuramaz**. Beklenenin tersi davranış.
- **Düzeltme:** Custom verifier:
  ```rust
  struct VerifyCaNoHostname { roots: RootCertStore }
  impl ServerCertVerifier for VerifyCaNoHostname {
      fn verify_server_cert(&self, end_entity, intermediates, _server_name, _ocsp, _now)
          -> Result<ServerCertVerified, TlsError>
      {
          // Manual chain verification, no hostname check
      }
  }
  ```
- **Test:** `verify_ca_accepts_hostname_mismatch`, `verify_ca_rejects_invalid_chain`.

---

## M2 ✅ — PG/MySQL `Require` TLS davranışı farklı

- **Dosya:**
  - `crates/narwhal-driver-postgres/src/tls.rs:62-67`
  - `crates/narwhal-driver-mysql/src/lib.rs:115-135`
- **Etki:** PG `Require` → AcceptAny (hiç doğrulama yok); MySQL `Require` →
  chain verify, hostname atla. Aynı isim, farklı tehdit modeli — kullanıcı
  driver'a göre farklı garanti alır.
- **Düzeltme:** H1'in tercih (a) ile PG `Require` MySQL ile aynı semantiğe gelir:
  chain doğrula, hostname atla (`VerifyCaNoHostname` re-use).
- **Bağımlı:** H1, M1.
- **Test:** `require_uses_chain_verifier_in_both_drivers`.

---

## M3 ✅ — `ssl_root_cert` set ama `ssl_mode == Disable` → sessiz yoksay

- **Dosya:** `crates/narwhal-driver-mysql/src/lib.rs:113-115`,
  `crates/narwhal-driver-postgres/src/tls.rs` (benzer durum)
- **Etki:** Kullanıcı `ssl_root_cert` ayarlamış ama `ssl_mode = Disable` → TLS
  hiç açılmaz, sertifika sessizce yoksayılır. Misconfig tespit edilmiyor.
- **Düzeltme:** `validate_connections` (narwhal-config):
  ```rust
  if params.ssl_mode == SslMode::Disable
      && (params.ssl_root_cert.is_some() || params.ssl_cert.is_some() || params.ssl_key.is_some())
  {
      return Err(Error::Config(
          "ssl_root_cert/ssl_cert/ssl_key set but ssl_mode = disable".into()));
  }
  ```
- **Test:** `disable_mode_with_ssl_files_rejected`.

---

## M4 ✅ — ClickHouse `escape_sql_string` backslash escape'lemiyor

- **Dosya:** `crates/narwhal-driver-clickhouse/src/lib.rs:496-498`
- **Etki:** ClickHouse `\'` escape'i onurlandırır; `'\\''` görüldüğünde string
  kapanmaz. Tablo adı `\` ile bitiyorsa sonraki token literal'e dahil olur —
  küçük injection vektörü.
- **Düzeltme:**
  ```rust
  fn escape_sql_string(s: &str) -> String {
      let mut out = String::with_capacity(s.len() + 2);
      for ch in s.chars() {
          match ch {
              '\\' => out.push_str("\\\\"),
              '\'' => out.push_str("''"),
              _ => out.push(ch),
          }
      }
      out
  }
  ```
  Aynı düzeltme `value_to_sql_literal`'in `String`, `Json`, `Unknown` kollarında.
- **Test:** `escape_handles_backslash`, `escape_handles_quote_then_backslash`.

---

## M5 ✅ — ClickHouse `Value::Float(NaN/Inf)` geçersiz literal

- **Dosya:** `crates/narwhal-driver-clickhouse/src/types.rs:240-249`
- **Etki:** `f64::NAN.to_string() == "NaN"`; ClickHouse `nan()` fonksiyonu
  bekler. Parametreli insert'te parse hatası.
- **Düzeltme:**
  ```rust
  Value::Float(f) if f.is_nan() => "nan()".into(),
  Value::Float(f) if f.is_infinite() => {
      if *f > 0.0 { "inf()".into() } else { "-inf()".into() }
  }
  Value::Float(f) => { /* existing path */ }
  ```
- **Test:** `bind_nan_renders_as_function`, `bind_neg_inf_renders_with_sign`.

---

## M6 ✅ — ClickHouse `cancel()` `drain()` ile set'i boşaltıyor

- **Dosya:** `crates/narwhal-driver-clickhouse/src/lib.rs:881-887`
- **Etki:** İkinci Ctrl-C no-op; eş zamanlı başlayan yeni sorgu drain anı
  geçtikten sonra eklendiyse ıskalanır.
- **Düzeltme:**
  ```rust
  let query_ids: Vec<String> = self.state.active_queries.lock().await
      .iter().cloned().collect();
  // drain YAPMA — sorgu tamamlandığında zaten remove ediliyor
  ```
- **Bağlı:** L2 (`tokio::sync::Mutex` → senkron).
- **Test:** `cancel_idempotent`, `cancel_kills_queries_added_after_first_call`.

---

## M7 ✅ — ClickHouse `stream` hata yolunda `query_id` leak

- **Dosya:** `crates/narwhal-driver-clickhouse/src/lib.rs:617-660`
- **Etki:** `task.abort()` task'ı async drop yapmaz; `active_queries.remove(&qid)`
  bekleyen `await`'ten önceyse çalışmayabilir → set'te ölü ID'ler birikir.
- **Düzeltme:** RAII guard:
  ```rust
  struct QueryGuard {
      active: Arc<Mutex<HashSet<String>>>,
      qid: String,
  }
  impl Drop for QueryGuard {
      fn drop(&mut self) {
          // sync (parking_lot::Mutex gerektirir — bkz. L2)
          self.active.lock().remove(&self.qid);
      }
  }
  ```
  Stream task'ında `let _guard = QueryGuard { ... };` kapsam dışına çıkınca
  her zaman temizler.
- **Bağlı:** L2.
- **Test:** `stream_error_path_clears_active_query`.

---

## M8 ✅ — PG `extract_csv` virgül içeren identifier'larda kırık

- **Dosya:** `crates/narwhal-driver-postgres/src/lib.rs:320-333`
- **Etki:** `string_agg(a.attname, ',')` + `split(',')` parse — `CREATE TABLE
  t ("a,b" int)` durumunda yanlış bölünür.
- **Düzeltme:**
  ```sql
  string_agg(a.attname, E'\x1F' ORDER BY k.ord)
  ```
  Sonra `.split('\x1F')` ile parse.
- **Alternatif:** `array_agg(a.attname ORDER BY k.ord)` ve PG array literal parse.
- **Test:** `extract_csv_handles_comma_in_identifier`.

---

## M9 ✅ — PG her `run()` çağrısında yeni `prepare` round-trip

- **Dosya:** `crates/narwhal-driver-postgres/src/lib.rs:336-385`
- **Etki:** `describe_table` tek tablo için 4 SQL × 2 round-trip = 8 RTT.
- **Düzeltme:** Şema/admin sorguları için `LruCache<String, Statement>`:
  ```rust
  struct PostgresConnection {
      client: Client,
      prepared: LruCache<String, Statement>,  // lru = "0.12"
      ...
  }

  async fn prepared(&mut self, sql: &str) -> Result<&Statement> {
      if !self.prepared.contains(sql) {
          let stmt = self.client.prepare(sql).await?;
          self.prepared.put(sql.to_owned(), stmt);
      }
      Ok(self.prepared.get(sql).unwrap())
  }
  ```
- **Boyut:** 64 makul başlangıç.
- **Test:** `prepared_cache_reuses_statement`, `prepared_cache_evicts_lru`.

---

## M10 ✅ — MySQL tek-kolonlu UNIQUE constraint'ler `len()>1` filtresiyle yutuluyor

- **Dosya:** `crates/narwhal-driver-mysql/src/lib.rs:425-431`
- **Etki:** `describe_table` UNIQUE constraint listesinde tek-kolon UNIQUE'ler
  yok; PG tarafında hepsi var → parity yok.
- **Düzeltme:**
  ```rust
  .filter(|i| i.unique && !i.primary)
  // .columns.len() > 1 filtresini kaldır
  ```
- **Test:** `describe_table_includes_single_column_unique`.

---

## M11 ✅ — SQLite/DuckDB/CH `describe_table` daima `TableKind::Table`

- **Dosya:**
  - `crates/narwhal-driver-sqlite/src/lib.rs:474-482`
  - `crates/narwhal-driver-duckdb/src/lib.rs:484-492`
  - `crates/narwhal-driver-clickhouse/src/lib.rs:769-773`
- **Etki:** View tıklandığında UI'da "Table" ikonu/etiketi gösterilir.
- **Düzeltme — SQLite:**
  ```rust
  let kind: String = conn.query_row(
      "SELECT type FROM sqlite_master WHERE name = ?1",
      [name], |r| r.get(0)
  )?;
  let kind = match kind.as_str() {
      "view" => TableKind::View,
      _ => TableKind::Table,
  };
  ```
- **DuckDB:** `duckdb_views` + `duckdb_tables` UNION query, kind kolonu döndür.
- **ClickHouse:** `system.tables.engine` `View`/`MaterializedView` ise View.
- **Test:** `describe_view_returns_view_kind` (her 3 driver).

---

## M12 ✅ — DuckDB Date32/Timestamp `"date(19876)"` string olarak render

- **Dosya:** `crates/narwhal-driver-duckdb/src/types.rs:62-72`
- **Etki:** UI'da DATE kolonu `"date(19876)"` olarak görünür — okunamaz.
- **Düzeltme:**
  ```rust
  ValueRef::Date32(days) => {
      let dt = chrono::NaiveDate::from_num_days_from_ce_opt(days + 719_163)
          .ok_or_else(|| Error::Conversion(format!("invalid date32: {days}")))?;
      Value::Date(dt)
  }
  ValueRef::Time64(unit, ticks) => {
      let ns = scaled(unit, ticks);
      let t = chrono::NaiveTime::from_num_seconds_from_midnight_opt(
          (ns / 1_000_000_000) as u32,
          (ns % 1_000_000_000) as u32
      ).ok_or(...)?;
      Value::Time(t)
  }
  ValueRef::Timestamp(unit, ticks) => {
      let ns = scaled(unit, ticks);
      let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(
          (ns / 1_000_000_000) as i64,
          (ns % 1_000_000_000) as u32
      ).ok_or(...)?;
      Value::Timestamp(dt)
  }
  ValueRef::Interval { months, days, nanos } => Value::Interval(...) // veya Value::String("P1M2DT3S")
  ```
- **Yan kazanım:** CSV export, clipboard, sort hepsi düzelir.
- **Test:** `date32_renders_as_iso_date`, `timestamp_renders_as_iso`.

---

## M13 ✅ — `Journal::recent` bloklayıcı sync + tüm dosyayı parse + sessiz yutma

- **Dosya:** `crates/narwhal-history/src/journal.rs:155-165`
- **Etki:** Tüm history dosyasını parse edip son N alıyor; UI thread'inden
  sync I/O. Parse hataları `filter_map(|r| r.ok())` ile yutuluyor.
- **Düzeltme:**
  ```rust
  pub async fn recent(&self, n: usize) -> Result<Vec<HistoryEntry>> {
      let path = self.path.clone();
      tokio::task::spawn_blocking(move || {
          let file = std::fs::File::open(&path)?;
          let size = file.metadata()?.len();
          // reverse line reader; n satır toplanınca dur
          let mut reader = rev_lines::RevLines::new(BufReader::new(file));
          let mut out = Vec::with_capacity(n);
          for line in reader.by_ref() {
              if out.len() >= n { break; }
              match serde_json::from_str::<HistoryEntry>(&line) {
                  Ok(e) => out.push(e),
                  Err(e) => tracing::warn!(error = %e, line = %line, "journal parse failed"),
              }
          }
          out.reverse();
          Ok(out)
      }).await?
  }
  ```
- **Dep:** `rev_lines = "0.3"` veya manuel reverse scan.
- **Test:** `recent_returns_last_n_in_chronological_order`,
  `recent_warns_on_corrupt_line`.

---

## M14 ✅ — `Connection`/`DatabaseDriver`/`Value`/`Outcome`/... `#[non_exhaustive]` değil

- **Dosya:**
  - `crates/narwhal-core/src/connection.rs:18-27, 68-75, 85-167`
  - `crates/narwhal-core/src/value.rs:11-25`
  - `crates/narwhal-core/src/schema.rs:11-17`
  - `crates/narwhal-history/src/journal.rs:23-29`
  - `crates/narwhal-sql/src/splitter.rs:9-21`
- **Etki:** v1.0 sonrası yeni varyant/metod eklemek SemVer breaking.
- **Düzeltme:** `#[non_exhaustive]` ekle. Trait'lerde her metod default impl
  almalı:
  ```rust
  #[non_exhaustive]
  pub enum Value { ... }

  pub trait Connection: Send {
      ...
      // New methods get default impl returning Error::unsupported(...)
      async fn list_all_tables(&mut self) -> Result<Vec<(Schema, Vec<Table>)>> {
          Err(Error::Unsupported("list_all_tables".into()))
      }
  }
  ```
- **Downstream impact:** İlk uygulamada kırılan match'ler crate-içi; CI yakalar.
- **Test:** Yok — derleme zamanı SemVer guard.

---

## M15 ✅ — Mouse table-preview cell-edit'i kaybediyor

- **Dosya:** `crates/narwhal-app/src/core.rs:1445-1458` vs `2080-2148`
- **Etki:** Sol klikle tabloya preview yapan kullanıcı cell edit (`e`)
  yapamıyor (`pending_source = None`); klavyeyle aynı tabloya gelen kullanıcı
  edit edebiliyor. Tutarsızlık.
- **Düzeltme:** `inject_table_preview`'ı `run_preview(&schema, &name, 0)`
  çağrısına yönlendir; `dispatch_current_statement` yolu kaldırılsın.
- **Test:** `mouse_click_preview_enables_cell_edit`.

---

## M16 ✅ — Vim operatörleri (`d`, `y`, `c`) state-machine'de yok

- **Dosya:** `crates/narwhal-vim/src/machine.rs:103-117`
- **Etki:** `Action::Operate { op, motion, count }` ve `Operator { Delete,
  Yank, Change }` enum'lar tanımlı, `handle_normal` üretmiyor. `d`, `y`, `c`
  tuşları `_ => Action::Pending` arm'ına düşüyor — kullanıcı hiçbir feedback
  almaz.
- **Düzeltme:** `Mode` enum'una `OperatorPending(Operator)` varyantı ekle;
  `handle_normal` `d/y/c` görünce mode değiştirsin, sonraki motion `Action::Operate`
  üretsin. Visual mode'da `d`/`y`/`c` selection'a uygulasın.
- **Test:**
  - `dd_deletes_line`
  - `yy_yanks_line`
  - `dw_deletes_word`
  - `c$_changes_to_end_of_line`
  - `visual_d_deletes_selection`

---

## M17 ✅ — `pending_count` overflow guard yok

- **Dosya:** `crates/narwhal-vim/src/machine.rs:56-60`
- **Etki:** `pending_count.unwrap_or(0) * 10 + digit` debug panik, release wrap.
  Yapışkan tuş veya kötü-niyetli script DoS vektörü.
- **Düzeltme:**
  ```rust
  const MAX_COUNT: u32 = 999_999; // vim real-world cap
  let next = self.pending_count.unwrap_or(0)
      .checked_mul(10).and_then(|v| v.checked_add(digit))
      .unwrap_or(MAX_COUNT)
      .min(MAX_COUNT);
  self.pending_count = Some(next);
  ```
- **Test:** `pending_count_clamps_to_max`.

---

## M18 ✅ — Plugin `_timeout_budget` script erişimine açık

- **Dosya:** `crates/narwhal-plugin-lua/src/lib.rs:323-326, 113`
- **Etki:** `narwhal._timeout_budget` field — script `nil` set ederek timeout
  budget'ı silebilir veya bozabilir.
- **Düzeltme:** Lua registry'ye taşı:
  ```rust
  lua.set_named_registry_value("narwhal_timeout_budget", secs)?;
  // read:
  let secs: f64 = lua.named_registry_value("narwhal_timeout_budget")?;
  ```
- **Test:** `script_cannot_clear_timeout_budget`, `set_timeout_still_works`.

---

## M19 ✅ — `LuaPlugin::from_path` `DefaultHasher` randomized — restart'ta farklı plugin adı

- **Dosya:** `crates/narwhal-plugin-lua/src/lib.rs:248-260`
- **Etki:** `DefaultHasher` randomized SipHash. Aynı dosya yolu her process'te
  farklı `lua-plugin-<hex>` adı üretir; log/catalogue tutarsız.
- **Düzeltme — basit:**
  ```rust
  let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("plugin");
  let name = format!("lua-{}", stem);
  ```
  Çakışma riski varsa `siphasher::sip128::SipHasher24::new_with_keys(0, 0)`
  ile deterministik hash kullan.
- **Test:** `plugin_name_deterministic_across_restarts`.

---

## M20 ✅ — `render_for_grid` BIDI/control karakter filtrelemiyor

- **Dosya:** `crates/narwhal-tui/src/widgets/results.rs:467-486`
- **Etki:** U+202A..U+202E, U+2066..U+2069 BIDI override karakterleri
  görsel kandırma yapabilir (Trojan source); kullanıcı sahte komut zannedebilir.
- **Düzeltme:** Sanitize helper:
  ```rust
  fn sanitize_for_grid(s: &str) -> Cow<str> {
      let needs = s.chars().any(|c| is_dangerous_glyph(c));
      if !needs { return Cow::Borrowed(s); }
      let mut out = String::with_capacity(s.len());
      for ch in s.chars() {
          if is_dangerous_glyph(ch) {
              out.push('·');  // veya '\u{FFFD}'
          } else { out.push(ch); }
      }
      Cow::Owned(out)
  }

  fn is_dangerous_glyph(c: char) -> bool {
      matches!(c,
          '\u{202A}'..='\u{202E}' |  // BIDI override
          '\u{2066}'..='\u{2069}' |
          '\u{200B}'..='\u{200F}' |  // zero-width, LRM/RLM
          '\u{0000}'..='\u{0008}' |
          '\u{000B}'..='\u{000C}' |
          '\u{000E}'..='\u{001F}' |
          '\u{007F}'                  // DEL
      )
  }
  ```
- **Çağrı yerleri:** `render_for_grid`, cell popup, row detail, history sql,
  sidebar label, status mesajı.
- **Test:**
  - `sanitize_replaces_bidi_override`
  - `sanitize_preserves_normal_unicode`
  - `cell_popup_sanitizes_input`

---

## M21 ✅ — TUI status bar `chars().count()` width hesabı yanlış

- **Dosya:** `crates/narwhal-tui/src/layout.rs:177-202`
- **Etki:** Mode/focus/conn/transaction etiketleri `chars().count() as u16`
  ile genişlik hesaplıyor; CJK adı, `⏳` (W=2), emoji'de alan yetmez veya boş
  kalır.
- **Düzeltme:**
  ```rust
  use unicode_width::UnicodeWidthStr;
  let w = text.width() as u16;
  ```
- **Test:** `status_bar_width_handles_wide_chars`.

---

## M22 ✅ — `ResultView.state: TableState` ratatui'yi public ihraç ediyor

- **Dosya:** `crates/narwhal-tui/src/widgets/results.rs:94-114`
- **Etki:** Ratatui major upgrade (`TableState` API değişimi) app'i kırar.
- **Düzeltme:** `pub state` → `pub(crate) state`; getter/setter sun:
  ```rust
  impl ResultView {
      pub fn select(&mut self, i: usize) { self.state.select(Some(i)); }
      pub fn selected(&self) -> Option<usize> { self.state.selected() }
      pub fn scroll(&mut self, offset: usize) { *self.state.offset_mut() = offset; }
  }
  ```
- **Bağımlı:** `narwhal-app` `state` direkt kullanan yerleri accessor'a geçer.
- **Test:** Mevcut testler accessor'la geçmeli.

---

## M23 ✅ — Magic number'lar 8+ widget dosyasında dağınık

- **Dosya:** `narwhal-tui/src/widgets/*.rs`, `layout.rs`
- **Düzeltme:** `narwhal-tui/src/layout.rs` (veya yeni `constants.rs`):
  ```rust
  pub mod constants {
      use std::time::Duration;
      use ratatui::layout::Constraint;

      pub const SIDEBAR_WIDTH: u16 = 34;
      pub const EDITOR_RESULTS_SPLIT: (Constraint, Constraint) =
          (Constraint::Percentage(55), Constraint::Percentage(45));
      pub const HELP_MODAL_MAX: (u16, u16) = (64, 50);
      pub const HISTORY_MODAL_MIN: (u16, u16) = (80, 24);
      pub const SNIPPETS_MODAL_MIN: (u16, u16) = (50, 20);
      pub const ROW_DETAIL_MIN: (u16, u16) = (80, 30);
      pub const COMPLETION_WIDTH_RANGE: (u16, u16) = (20, 100);
      pub const COMPLETION_MAX_HEIGHT: u16 = 10;
      pub const STREAM_RENDER_THROTTLE: Duration = Duration::from_millis(100);
      pub const SCHEMA_REFRESH_DEBOUNCE: Duration = Duration::from_millis(200);
      pub const HISTORY_LOAD_LIMIT: usize = 200;
  }
  ```
- **Test:** Yok — refactor, davranış parity.

---

# LOW

## L1 ✅ — `Value::render` Display path'inde alloc

- **Dosya:** `crates/narwhal-core/src/value.rs:32-49, 52-56`
- **Düzeltme:** `Display::fmt` doğrudan formatter'a yaz:
  ```rust
  impl fmt::Display for Value {
      fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
          match self {
              Value::Null => write!(f, "NULL"),
              Value::String(s) => f.write_str(s),
              Value::Int(i) => write!(f, "{i}"),
              // ...
          }
      }
  }
  pub fn render(&self) -> String { self.to_string() }
  ```

## L2 ✅ — ClickHouse `tokio::sync::Mutex` gereksiz

- **Dosya:** `crates/narwhal-driver-clickhouse/src/lib.rs:258-265`
- **Düzeltme:** `parking_lot::Mutex<HashSet<String>>` veya `std::sync::Mutex`;
  lock'lar `.await` aşmıyor.

## L3 ✅ — `Splitter::find_dollar_close` lineer

- **Dosya:** `crates/narwhal-sql/src/splitter.rs:118-128`
- **Düzeltme:** `memchr::memmem::find` ile O(n).

## L4 ✅ — `parse_url` IPv6 desteklemiyor

- **Dosya:** `crates/narwhal-config/src/url.rs:96-107`
- **Düzeltme:** `[...]` bracket parsing.

## L5 ✅ — `validate_connections` UUID benzersizlik check yok

- **Dosya:** `crates/narwhal-config/src/settings.rs:136-180`

## L6 ✅ — `PoolConfig::max_size = 0` deadlock

- **Dosya:** `crates/narwhal-pool/src/pool.rs:18-32, 56-69`
- **Düzeltme:** `Pool::new`'da `max_size > 0` assert veya `Result`.

## L7 ✅ — `parse_query` boş anahtar kabul ediyor

- **Dosya:** `crates/narwhal-config/src/url.rs:178-191`

## L8 ✅ — `Param` `i16`/`i32`/`u32` overflow sessiz

- **Dosya:** `crates/narwhal-driver-postgres/src/types.rs:31-37`
- **Düzeltme:** `try_from`.

## L9 ✅ — DDL generated `unwrap_or("")` boş ifade

- **Dosya:** `crates/narwhal-driver-postgres/src/ddl.rs:55-64`

## L10 ✅ — DuckDB `_tx` doğrudan drop edilebilir

- **Dosya:** `crates/narwhal-driver-clickhouse/src/lib.rs:597-606`

## L11 ✅ — SQLite/DuckDB path canonical log

- **Dosya:** `crates/narwhal-driver-sqlite/src/lib.rs:79-95`,
  `crates/narwhal-driver-duckdb/src/lib.rs:85-101`
- **Düzeltme:** `validate()`'de `path.canonicalize()` ve UI'da tam yol göster.

## L12 ✅ — Plugin `editor_text` doc (mevcut doc doğru)

- **Dosya:** `crates/narwhal-plugin-lua/src/lib.rs:317-322`

## L13 ✅ — `KeyMod`/`Mode` `Hash` derive yok

- **Dosya:** `crates/narwhal-vim/src/key.rs:57-72, mode.rs:5-10`

## L14 ✅ — `command_buffer` boyut sınırı yok

- **Dosya:** `crates/narwhal-vim/src/machine.rs:139-141`

## L15 ✅ — `editor.rs:594` ölü `.max` guard

- **Dosya:** `crates/narwhal-tui/src/widgets/editor.rs:594`

## L16 ✅ — `EditorBuffer::move_word_forward` newline atlamıyor

- **Dosya:** `crates/narwhal-tui/src/widgets/editor.rs:392-403`

## L17 ✅ — `wrap_text` byte-chunk fallback

- **Dosya:** `crates/narwhal-tui/src/widgets/row_detail.rs:165-178, 184-187`

## L18 ✅ — `format_count(999_999)` → `1000.0k`

- **Dosya:** `crates/narwhal-tui/src/widgets/results.rs:407-414`

## L19 ✅ — `format_elapsed(59_999ms)` → `60.0s`

- **Dosya:** `crates/narwhal-tui/src/widgets/results.rs:417-425`

## L20 ✅ — `narwhal/src/main.rs` `_settings` kullanılmıyor + `unwrap_or_default` sessiz

- **Dosya:** `narwhal/src/main.rs:33-40`
- **Düzeltme:** Hata logla, `_settings`'i `App::with_services`'a ilet veya kaldır.

## L21 ✅ — `core.rs` 4858 satır (modül bölme)

- **Dosya:** `crates/narwhal-app/src/core.rs`
- **Düzeltme:** `core/{results,tabs,run_loop,transactions,plugins}.rs`.

## L22 ✅ — `expect("plugin_state poisoned")` × 6

- **Dosya:** `crates/narwhal-app/src/core.rs:3447,3448,3464,3660,3730,3876`
- **Düzeltme:** `lock().unwrap_or_else(|e| e.into_inner())`.

## L23 ⏯️ Wave 6 — `Tab` field'ları `pub` (API yüzeyi) — bkz. plans/wave-6-followups.md

- **Dosya:** `crates/narwhal-app/src/core.rs`

## L24 ⏯️ Wave 6 — Sidebar scroll yok — bkz. plans/wave-6-followups.md

- **Dosya:** `crates/narwhal-tui/src/widgets/sidebar.rs:99-114`

## L25 ✅ — `centred_rect` DRY ihlali (4 dosya)

- **Dosya:** `widgets/row_detail.rs`, `results.rs`, `wizard.rs`, `help.rs`
- **Düzeltme:** Tek `centred_rect` helper'da topla.

## L26 ✅ — `widgets.rs` ve `lib.rs` re-export çiftlemesi

- **Dosya:** `crates/narwhal-tui/src/{lib,widgets}.rs`

## L27 ✅ — `Pane::cycle` tek yön

- **Dosya:** `crates/narwhal-tui/src/layout.rs:46-52`

## L28 ⏯️ Wave 6 — `ClickHouse` query_tsv tüm gövdeyi materialize — bkz. plans/wave-6-followups.md

- **Dosya:** `crates/narwhal-driver-clickhouse/src/lib.rs:451-462`
- **Düzeltme:** `execute()` belirli eşik üstü stream'e geçsin veya doc'a uyarı.

## L29 ✅ — `value_from_my` Bytes her zaman UTF-8 dener (BLOB → String)

- **Dosya:** `crates/narwhal-driver-mysql/src/types.rs:55-62`
- **Düzeltme:** `column.column_type()` BLOB/VARBINARY ise zorla `Value::Bytes`.

## L30 ✅ — MySQL view tipi `describe_table`'da işaretlenmiyor (M11'in MySQL eşi)

- **Dosya:** `crates/narwhal-driver-mysql/src/lib.rs:367-447`

## L31 ⏯️ Wave 6 — MySQL `KILL QUERY` cancel desteği yok — bkz. plans/wave-6-followups.md

- **Dosya:** `crates/narwhal-driver-mysql/src/lib.rs:494-496`

## L32 ✅ — `find_all` ölü `.max(1)`

- **Dosya:** `crates/narwhal-app/src/core.rs:4798-4807`

## L33 ✅ — İlk tab adı `untitled` (next `untitled-2`)

- **Dosya:** `crates/narwhal-app/src/core.rs:589, 3996-4000`

## L34 ✅ — `parse_input` veri tipi tahmini agresif (`"true"` → bool)

- **Dosya:** `crates/narwhal-app/src/edit.rs:23-46`

## L35 ✅ — `narwhal-tui` `tracing` dep var, kullanılmıyor

- **Dosya:** `crates/narwhal-tui/Cargo.toml`

## L36 ✅ — `GUTTER_WIDTH` sabit 6 (>999 satır taşar)

- **Dosya:** `crates/narwhal-tui/src/widgets/editor.rs:17`

## L37 ✅ — `core::ConfigPaths::ensure` path-aware hata yok

- **Dosya:** `crates/narwhal-config/src/paths.rs:54-61`

## L38 ✅ — `HistoryEntry::sql` boyut sınırı yok

- **Dosya:** `crates/narwhal-history/src/journal.rs:32-46, 142-151`
- **Düzeltme:** 64KB üstü truncate + ekle `"… (truncated N bytes)"`.

## L39 ✅ — `Pool::idle_count` poison'da 0 dönüyor

- **Dosya:** `crates/narwhal-pool/src/pool.rs`

## L40 ✅ — `process::exit(1)` log flush kaçırıyor

- **Dosya:** `narwhal/src/main.rs`
- **Düzeltme:** `drop(_guard)` öncesi açıkça flush veya `return Err`.

---

# Pozitif Notlar (Regression Korumak İçin)

Bu noktalara dokunan değişiklikler **testleri kırarsa fix yanlış**:

- ClickHouse TSV byte-doğru yeniden yazımı (`parse_tsv_value` `&[u8]`,
  `b"\\N"` check'i escape'ten önce, invalid UTF-8 → `Value::Bytes`).
- Mid-row truncation `Error::Query` yüzeyi.
- SQLite/DuckDB view-aware DDL (`type IN ('table','view')` /
  `duckdb_tables UNION ALL duckdb_views`).
- Run-tab pinning + `Acquire/Release` atomic ordering — `tests/run_tab_pinning.rs`.
- Command shadowing reddi (built-in adlar plugin'lerce shadow edilemez).
- `:begin` altında `sql_run` reddi.
- Duration overflow guard (`read_timeout_budget` `1e308`, `0/0`, `-5`).
- Transform chain failure resilience (TransformErrors collect, partial result korunur).
- `Capabilities` `#[non_exhaustive]` (mevcut).
- Keyring backend feature flag'leri (`apple-native`/`windows-native`/...) — regression testi var.
- TLS validation (cert/key parity, file driver guardrails, verify-* root_cert zorunluluğu) —
  `crates/narwhal-config/tests/tls.rs`.
- Splitter dialect-aware temel akış (PG `$tag$ ... $tag$`, MySQL backtick, nested block comment).
- `Journal::append` mutex + `O_APPEND` satır-sınır