# Plan 07-09 — Byte-accurate row tests for every driver

## Why

Plan 05 ported ClickHouse to byte-accurate row mapping with a
thorough test set covering NULL/empty disambiguation, invalid
UTF-8, embedded NULs, tab/newline-in-string, and numeric edges.

The other four drivers (postgres, mysql, sqlite, duckdb) don't
have the same coverage. A regression there is exactly the kind
of bug that surfaces in production three months after the user
trusted narwhal with their data and finds out the hard way that
the binary column wasn't really binary-safe.

## Scope

For each of **postgres**, **mysql**, **sqlite**, **duckdb**: a
test module that asserts five invariants per driver, plus the
existing ClickHouse module gets a pass to make sure the same
test scaffold runs there too (consistency).

Invariants:

1. **NULL ≠ empty string** — `INSERT (a, b) VALUES (NULL, '')`,
   round-trip, assert `a` is `Value::Null` and `b` is `Value::String("")`.
2. **Invalid UTF-8 survives as `Value::Bytes`** — write a row
   with bytes `b"\xff\xfe\xfd"` into a BLOB / BYTEA / VARBINARY
   column, round-trip, assert the read value is `Value::Bytes`
   with the same bytes.
3. **Embedded `\0` survives** — write a string with an embedded
   `\0` byte (where the column type supports it: BYTEA / BLOB),
   round-trip, assert no truncation.
4. **Tab/newline-in-string survives** — write `"col\twith\nnewline"`,
   round-trip, assert byte equality.
5. **Numeric edges**:
   - `i64::MAX` and `i64::MIN` round-trip.
   - `f64::INFINITY` / `f64::NEG_INFINITY` / `f64::NAN` either
     round-trip as themselves or reject with a clear error; no
     silent lossy conversion to a finite value.

## Constraints

- AGENTS.md: no `unwrap()` / `expect()` in production code
  (tests are exempt — `expect("test wiring")` is fine in test
  setup).
- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- One conventional commit, long-form.
- **CI gating**: postgres / mysql tests skip if a `NARWHAL_*_URL`
  env var isn't set; sqlite + duckdb always run (file-local);
  clickhouse uses the existing harness.
- Each test module uses a per-driver helper (`test_connect()`)
  so the wiring stays per-driver.

## Concrete steps

### Step 1: per-driver byte-test files

Five new test files:

- `crates/narwhal-driver-postgres/tests/byte_accuracy.rs`
- `crates/narwhal-driver-mysql/tests/byte_accuracy.rs`
- `crates/narwhal-driver-sqlite/tests/byte_accuracy.rs`
- `crates/narwhal-driver-duckdb/tests/byte_accuracy.rs`
- `crates/narwhal-driver-clickhouse/tests/byte_accuracy.rs`
  (consolidates / supplements existing tests)

Each file pattern:

```rust
#[tokio::test]
async fn null_vs_empty_string() -> Result<()> {
    let Some(conn) = test_connect().await? else { return Ok(()); };
    conn.execute("DROP TABLE IF EXISTS narwhal_byte_test").await?;
    conn.execute("CREATE TABLE narwhal_byte_test (a TEXT, b TEXT)").await?;
    conn.execute("INSERT INTO narwhal_byte_test VALUES (NULL, '')").await?;
    let rows = conn.query("SELECT a, b FROM narwhal_byte_test").await?;
    assert_eq!(rows[0].0[0], Value::Null);
    assert_eq!(rows[0].0[1], Value::String("".into()));
    Ok(())
}
```

For BLOB-type columns (`bytea`, `BLOB`, etc.), the binary
invariants run against a separate test table.

### Step 2: test_connect helper

Per-driver `tests/common/mod.rs`:

```rust
pub async fn test_connect() -> Result<Option<Box<dyn Driver>>> {
    let url = std::env::var("NARWHAL_POSTGRES_URL").ok();
    let Some(url) = url else { return Ok(None); };
    // ... build driver
    Ok(Some(driver))
}
```

`None` → test does its setup-and-skip pattern; the test still
runs but asserts nothing concrete (cargo reports it as passing).

For sqlite + duckdb, always use a tempfile (`tempfile` crate is
already in workspace).

### Step 3: invalid-UTF8 path

The most subtle invariant — the driver must map invalid UTF-8
to `Value::Bytes`, not `Value::String(lossy)`. Check the existing
driver's row-mapping code for each driver and add a fallback if
missing:

```rust
match std::str::from_utf8(bytes) {
    Ok(s) => Value::String(s.into()),
    Err(_) => Value::Bytes(bytes.to_vec()),
}
```

Postgres already mostly does this for BYTEA; verify and patch
for TEXT columns too if needed.

### Step 4: tests

Twelve assertions split across the four drivers:

| Driver     | Tests |
|------------|-------|
| postgres   | 3 (null/empty, invalid-utf8 in TEXT, numeric edges) |
| mysql      | 3 (null/empty, invalid-utf8 in VARBINARY, numeric edges) |
| sqlite     | 3 (null/empty, invalid-utf8 in BLOB, numeric edges) |
| duckdb     | 3 (null/empty, invalid-utf8 in BLOB, numeric edges) |

(Embedded `\0` and tab/newline cases are covered as sub-asserts
inside the relevant per-driver test for brevity — the test
count is one per driver per invariant family.)

Acceptance: +12 tests.

## Files

- `crates/narwhal-driver-postgres/tests/byte_accuracy.rs` (new)
- `crates/narwhal-driver-postgres/tests/common/mod.rs` (new)
- `crates/narwhal-driver-mysql/tests/byte_accuracy.rs` (new)
- `crates/narwhal-driver-mysql/tests/common/mod.rs` (new)
- `crates/narwhal-driver-sqlite/tests/byte_accuracy.rs` (new)
- `crates/narwhal-driver-duckdb/tests/byte_accuracy.rs` (new)
- Driver crates: any row-mapping fixes needed to make the tests
  pass (likely: `Value::String` → `Value::Bytes` fallback for
  invalid UTF-8 in postgres/mysql TEXT columns).

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean
- `nix develop --command cargo test --all` reports +12 from
  baseline (sqlite + duckdb tests run on every dev box; postgres
  + mysql skip when env unset)
- The skipping pattern uses `if test_connect == None { return }`,
  not `#[ignore]`, so the test count visibly matches whether
  the env is set.

## Commit message template

```
test(drivers): byte-accurate row invariants for every driver

Plan 05 covered ClickHouse with a thorough byte-accurate test
set; the other four drivers had no equivalent coverage.  A
regression there is exactly the kind of bug that surfaces three
months after a user trusts narwhal with binary data and finds
out the hard way that the column wasn't really binary-safe.

Five invariants per driver:

1. NULL ≠ empty string — Value::Null vs Value::String("")
2. Invalid UTF-8 survives as Value::Bytes — not lossy String
3. Embedded \\0 survives in BYTEA / BLOB / VARBINARY
4. Tab/newline in string survives byte-exact
5. Numeric edges — i64::MAX/MIN exact, f64 NaN/Inf either exact
   or rejection (no silent lossy)

Per-driver test files use a test_connect() helper that returns
None when the relevant NARWHAL_*_URL env var is unset, so the
postgres / mysql / clickhouse tests skip gracefully in
environments without a live DB while sqlite / duckdb always
run (file-local via tempfile).

Drive-by fixes where the row-mapping code wasn't already routing
invalid UTF-8 to Value::Bytes — the test set forced the question
and the answer was "fall back from str::from_utf8 to bytes".

Twelve new tests total (three per driver across postgres, mysql,
sqlite, duckdb).  ClickHouse's existing test set was supplemented
to use the same naming scheme so the byte_accuracy suite is
consistent across the workspace.
```
