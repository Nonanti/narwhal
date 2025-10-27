# Plan 05 — ClickHouse: byte-accurate TSV decoding

## Why

Two correctness bugs that compound:

1. **`String::from_utf8_lossy` silent data loss.**
   `crates/narwhal-driver-clickhouse/src/lib.rs` and `src/types.rs`
   both convert TSV response bytes to `String` before parsing. The
   buffered path uses `reqwest::Response::text()`; the streaming
   path uses `String::from_utf8_lossy(&line_bytes)`. Both replace
   any invalid UTF-8 sequence with `U+FFFD REPLACEMENT CHARACTER`,
   so ClickHouse `String` cells that legitimately contain arbitrary
   bytes (the ClickHouse `String` type is byte-oriented — it stores
   any byte sequence, not just UTF-8 text) come back corrupted with
   no warning.

2. **TSV escape sequences are never decoded.**
   `parse_tsv_value` does `Value::String(raw.to_owned())`, where
   `raw` is the field text exactly as it appeared on the wire. But
   ClickHouse's TSV format escapes `\b \f \n \r \t \0 \\ \'` in
   string cells (see
   <https://clickhouse.com/docs/en/interfaces/formats#tabseparated>).
   A cell whose actual value is `"line1\nline2"` arrives as the
   four characters `line1\nline2` (backslash + lowercase n), and
   we store the four-character literal in `Value::String` instead
   of the two-line original. Any downstream consumer that copies
   the cell to the clipboard, writes it to a CSV, or compares it
   with a literal sees garbage.

## Constraints

- Behaviour-preserving for **all current passing tests**. The
  existing `parse_tsv_body` tests use ASCII-only payloads where
  `as_bytes()` and the previous `&str` path produce identical
  results — they must keep passing.
- New error path for invalid UTF-8 in numeric/bool/UUID cells is
  fine (those types are always ASCII on the wire); the new
  behaviour for `String` cells is "preserve as `Value::Bytes` if
  not valid UTF-8 after escape decoding."
- `clippy --all-targets -- -D warnings` clean, `fmt --check` clean.
- AGENTS.md: no `unwrap`/`expect` in production code.
- One commit, conventional, long-form.
- NixOS host: every cargo invocation through `nix develop --command`.

## Concrete steps

### Step 1: TSV escape decoder

Add a private helper in `types.rs`:

```rust
/// Decode ClickHouse TSV escape sequences in a string-typed field.
/// Returns the decoded bytes (which may not be valid UTF-8).
///
/// ClickHouse escapes `\b \f \n \r \t \0 \\ \'` in TSV string cells.
/// Any other byte is passed through unchanged — including bytes that
/// are not valid UTF-8.
fn decode_tsv_string_bytes(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] == b'\\' && i + 1 < field.len() {
            let next = field[i + 1];
            let decoded = match next {
                b'b' => Some(0x08),
                b'f' => Some(0x0C),
                b'n' => Some(b'\n'),
                b'r' => Some(b'\r'),
                b't' => Some(b'\t'),
                b'0' => Some(0x00),
                b'\\' => Some(b'\\'),
                b'\'' => Some(b'\''),
                _ => None,
            };
            if let Some(byte) = decoded {
                out.push(byte);
                i += 2;
                continue;
            }
        }
        out.push(field[i]);
        i += 1;
    }
    out
}
```

Note: the two-character sequence `\N` for NULL is handled *before*
this decoder runs (NULL check stays unchanged).

### Step 2: `parse_tsv_value` takes `&[u8]`

Change the signature to `pub(crate) fn parse_tsv_value(raw: &[u8],
ch_type: &str) -> Value`. Implementation outline:

- NULL: `if raw == b"\\N" { return Value::Null; }`
- Numeric/Bool/Uuid: `std::str::from_utf8(raw)` strict. On
  `Err(_)`: `Value::Unknown(String::from_utf8_lossy(raw).into_owned())`.
  Then parse the resulting `&str` exactly as today.
- String:
  - If `raw.is_empty() && is_nullable_type(ch_type)` → `Value::Null`.
  - Otherwise: `let decoded = decode_tsv_string_bytes(raw);`
  - `match String::from_utf8(decoded) { Ok(s) => Value::String(s), Err(e) => Value::Bytes(e.into_bytes()) }`.

`from_utf8` (not `from_utf8_lossy`) is the key change for the
String path — it lets us route invalid UTF-8 to `Value::Bytes`
instead of silently mangling it.

### Step 3: `parse_tsv_body` takes `&[u8]`

Change the signature to `pub(crate) fn parse_tsv_body(body: &[u8])
-> (Vec<String>, Vec<String>, Vec<Vec<Value>>)`.

- Header lines: the column-name and type-string lines are always
  UTF-8 (ClickHouse generates them as ASCII identifiers and type
  names). Use `std::str::from_utf8(line).unwrap_or("")` defensively
  and split on `b'\t'` at byte level. Push `String`s into the
  `headers` / `type_strings` vectors as before.
- Data rows: split each line on `b'\t'` at byte level, get `&[u8]`
  slices, hand them to the new byte-taking `parse_tsv_value`.

Replace `body.lines()` with a manual byte-level line splitter:

```rust
fn split_lines(body: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in body.iter().enumerate() {
        if b == b'\n' {
            let mut end = i;
            if end > start && body[end - 1] == b'\r' {
                end -= 1;
            }
            out.push(&body[start..end]);
            start = i + 1;
        }
    }
    if start < body.len() {
        // Trailing line without LF.
        let mut end = body.len();
        if end > start && body[end - 1] == b'\r' {
            end -= 1;
        }
        out.push(&body[start..end]);
    }
    out
}
```

The empty-line skip and "pad missing fields with Null" loops stay
unchanged.

### Step 4: `lib.rs` buffered path uses bytes

In `http_query`, today's return type is `Result<String>` and the
caller (`query_tsv`) does `parse_tsv_body(&body)`.

Change `http_query` to return `Result<Vec<u8>>`:

```rust
response.bytes().await
    .map(|b| b.to_vec())
    .map_err(|e| Error::Query(e.to_string()))
```

And `query_tsv` calls `parse_tsv_body(&body)` where `body: Vec<u8>`.

DDL paths (`execute_raw`) don't read the body — only the status —
so they don't care about the change. The error path's
`response.text().await.unwrap_or_default()` keeps using `text()`
because error bodies *are* expected to be UTF-8 (they're ClickHouse
error messages).

### Step 5: `stream_tsv_chunks` keeps line bytes as bytes

In the row-mode loop, today:

```rust
let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
let line = String::from_utf8_lossy(&line_bytes);
let line = line.trim_end_matches('\n').trim_end_matches('\r');
if line.is_empty() { continue; }
let fields: Vec<&str> = line.split('\t').collect();
```

Replace with byte-level split:

```rust
let mut line_bytes: Vec<u8> = buf.drain(..=pos).collect();
// Strip trailing \n and optional \r.
if line_bytes.last() == Some(&b'\n') { line_bytes.pop(); }
if line_bytes.last() == Some(&b'\r') { line_bytes.pop(); }
if line_bytes.is_empty() { continue; }
let fields: Vec<&[u8]> = line_bytes.split(|&b| b == b'\t').collect();
let mut row = Vec::with_capacity(headers.len());
for (i, field) in fields.iter().enumerate() {
    let ch_type = type_strings.get(i).map(String::as_str).unwrap_or("String");
    row.push(parse_tsv_value(field, ch_type));
}
```

The header collection phase: header lines *are* expected to be
UTF-8 (column identifiers and type strings). Keep the existing
`String::from_utf8_lossy` for those two lines only — corruption
there would be a server bug, not user data, and we'd want a noisy
result anyway. Add a `tracing::warn!` if `from_utf8_lossy` actually
substituted anything (`String::from_utf8(line_bytes.clone())` Err
path).

Trailing-line flush at end-of-stream: same byte-level treatment as
the in-buffer rows.

## Files

- `crates/narwhal-driver-clickhouse/src/types.rs` (decoder helper,
  signature changes, byte-level body splitter, all existing tests
  updated to pass `body.as_bytes()`).
- `crates/narwhal-driver-clickhouse/src/lib.rs` (`http_query` return
  type, `query_tsv` body handling, `stream_tsv_chunks` byte-level
  row parsing).

## Tests

Add the following to `types.rs`:

1. `parse_tsv_escape_decoded_string`: payload contains
   `line1\\nline2` (six characters), assert the parsed value is
   `Value::String("line1\nline2")` (two lines, 11 bytes).
2. `parse_tsv_string_preserves_invalid_utf8`: payload contains the
   single byte `0xFF`, assert the parsed value is
   `Value::Bytes(vec![0xFF])`.
3. `parse_tsv_string_decodes_all_known_escapes`: a single field
   containing `\\b\\f\\n\\r\\t\\0\\\\\\'`, assert the decoded bytes
   are `[0x08, 0x0C, 0x0A, 0x0D, 0x09, 0x00, 0x5C, 0x27]`.
4. `parse_tsv_string_preserves_unknown_backslash_sequences`: a
   field with `\\x` (backslash-x, not in the escape table); the
   decoder must pass both bytes through unchanged
   (`Value::String("\\x")`).

Update `parse_full_tsv_body` and `parse_tsv_body_with_null` to
call `parse_tsv_body(body.as_bytes())` instead of
`parse_tsv_body(body)`.

Add to `stream_tests` in `lib.rs`:

5. `chunked_tsv_preserves_binary_string`: a 1-row stream where the
   single String cell is `0xFF 0xFE 0x00 0x01`, assert the row's
   first value is `Value::Bytes(vec![0xFF, 0xFE, 0x00, 0x01])`.

Acceptance: total test count **199 → 204** (+5).

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` reports **204** passed.
- Module-level doc: extend the "Streaming" section to mention
  TSV escape decoding and byte preservation.

## Commit message template

```
fix(driver-clickhouse): byte-accurate TSV decoding

Two correctness bugs that compounded:

1. response.text() and from_utf8_lossy(&line_bytes) silently
   converted invalid UTF-8 to U+FFFD. ClickHouse's String type
   is byte-oriented — it stores any byte sequence — so cells
   carrying real binary payloads (image thumbnails stored as
   String, gzip-compressed JSON, ...) came back as garbage with
   no warning.

2. parse_tsv_value did Value::String(raw.to_owned()) without
   ever decoding ClickHouse's TSV escape sequences. A cell whose
   actual value was "line1\nline2" arrived as the literal six
   characters "line1\\nline2" and was stored verbatim.

The fix is a single end-to-end refactor toward byte-level
parsing:

- New decode_tsv_string_bytes() turns escape sequences into the
  bytes they represent (\\b \\f \\n \\r \\t \\0 \\\\ \\' per the
  ClickHouse TSV spec).
- parse_tsv_value() now takes &[u8] and decides between
  Value::String and Value::Bytes based on whether the decoded
  bytes are valid UTF-8 (strict from_utf8, not lossy).
- parse_tsv_body() takes &[u8] and walks lines/fields at the
  byte level so nothing routes through &str on the data path.
- http_query() returns Vec<u8> and query_tsv() feeds those
  bytes to parse_tsv_body() directly.
- stream_tsv_chunks() row mode splits the line buffer on byte
  boundaries and hands &[u8] field slices straight to
  parse_tsv_value() — no per-row from_utf8_lossy().

Header lines (column names + type strings) keep going through
String because they are always ASCII identifiers on the wire;
the byte-level path applies only to data cells where the user's
data lives.

Numeric/Bool/Uuid cells use strict std::str::from_utf8() and
route invalid UTF-8 to Value::Unknown — those types are ASCII
on the wire so invalid bytes there indicate a server bug worth
surfacing rather than papering over.

Five new tests cover the round-trip: escape decoding, invalid
UTF-8 preservation, the full known-escape table, unknown
backslash sequences pass through, and a streaming binary cell
arrives as Value::Bytes. Total test count 204.
```
