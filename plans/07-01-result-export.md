# Plan 07-01 — Result export (CSV / JSON / INSERT)

## Why

Plan 06-09 added Tab-completion for `:export <fmt> <path>` but
the actual writer doesn't exist — pressing Enter today produces
"unknown command". Users can't get a query result out of narwhal
to disk; they fall back to the database's own export tooling.

The `csv_export.lua` example plugin copies a single row to the
clipboard. That's helpful for ad-hoc cell yanking, not for
exporting a 100k-row result.

## Scope

New core command `:export <fmt> <path>` where `<fmt>` ∈
{ `csv`, `json`, `insert` }:

- `csv`     RFC 4180. Header row from result columns. Quote
            cells that contain `,`, `"`, `\r`, `\n`. NULL → empty
            field. Embedded `"` → `""`. Newline = CRLF.
- `json`    Array of objects, one per row. NULL → `null`. Numbers
            stay numeric (not stringified). Bytes that aren't
            valid UTF-8 → base64 string with a sentinel key
            `{"$bytes": "..."}` (rare; documents the round-trip).
- `insert`  `INSERT INTO <table> (col1, col2, ...) VALUES (...);`
            one statement per row. Requires the result set to
            carry a known source table (single-table `SELECT *
            FROM x` or DDL that names a target); otherwise the
            command sets a status message and writes nothing.

Streams to disk — uses a `BufWriter<File>` and writes rows as
they're consumed, so a 10M-row export doesn't sit in memory.

Respects an active filter / sort from plan 06-04: the exported
rows are the *visible* rows in the result pane at the moment
`:export` is invoked.

## Constraints

- AGENTS.md: no `unwrap()` / `expect()` in production code.
- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- One conventional commit, long-form body.
- No new heavy dependencies: `csv` crate (already in workspace
  via clickhouse driver tests, lift to dev-dep → runtime-dep on
  narwhal-app), `serde_json` (already in workspace), `base64`
  (existing).

## Concrete steps

### Step 1: ExportFormat enum

`crates/narwhal-app/src/export.rs` (new):

```rust
pub enum ExportFormat { Csv, Json, Insert }

pub struct ExportRequest {
    pub format: ExportFormat,
    pub path: PathBuf,
    pub source_table: Option<QualifiedName>,
}

pub fn write_csv<W: Write>(w: &mut W, cols: &[ColumnHeader], rows: &[Row]) -> Result<()>
pub fn write_json<W: Write>(w: &mut W, cols: &[ColumnHeader], rows: &[Row]) -> Result<()>
pub fn write_insert<W: Write>(w: &mut W, table: &QualifiedName, cols: &[ColumnHeader], rows: &[Row]) -> Result<()>
```

### Step 2: parse the prompt

`commands.rs` gains:

```rust
Command::Export { format: ExportFormat, path: PathBuf },
```

Parser handles `csv` / `json` / `insert`, rejects others with a
descriptive error.

### Step 3: dispatch

`AppCore::run_command` adds an Export branch that:
1. Resolves the visible-rows projection (filter + sort applied)
2. Resolves the source table from `ResultState::Rows.source_table`
   (new optional field — set on single-table SELECT, None
   otherwise)
3. Opens a `BufWriter<File>` at the requested path
4. Calls the matching writer
5. Posts a status message: `exported N rows to <path>`

### Step 4: source_table tracking

`run.rs` and `session.rs` already build a `ResultState::Rows`
when a query lands. Add an `Option<QualifiedName>` field that's
populated when the dispatched SQL parses (loosely) to a single-
table `SELECT ... FROM <ident>` and is `None` otherwise. Don't
add a real SQL parser — a regex-based heuristic is fine and the
INSERT branch rejects when source_table is None.

## Files

- `crates/narwhal-app/src/export.rs` (new, ~250 lines)
- `crates/narwhal-app/src/lib.rs` (export the module)
- `crates/narwhal-app/src/commands.rs` (`Command::Export`)
- `crates/narwhal-app/src/core.rs` (dispatch + source_table field)
- `crates/narwhal-app/src/run.rs` (populate source_table)
- `crates/narwhal-app/Cargo.toml` (`csv` crate runtime-dep)
- `crates/narwhal-app/tests/export.rs` (new)

## Tests

`tests/export.rs`:

1. `csv_round_trip_with_special_chars` — a row containing `,`,
   `"`, newline; assert the parsed CSV equals the original
   values.
2. `csv_null_becomes_empty_field` — NULL exports as empty.
3. `json_array_of_objects` — emit + parse → equal.
4. `json_invalid_utf8_uses_bytes_sentinel` — Value::Bytes that
   isn't valid UTF-8 emits `{"$bytes": "..."}`.
5. `insert_single_table_round_trip` — `SELECT * FROM users`,
   export INSERT, exported statements parse back to the same
   rows when fed to sqlite.
6. `insert_without_source_table_errors` — `SELECT 1+1`, attempt
   export INSERT, assert status message + file not created.

Acceptance: +6 tests.

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean
- `nix develop --command cargo test --all` reports +6 from baseline
- Manual smoke: `:export csv /tmp/out.csv` after a query writes
  the file; `cat /tmp/out.csv` is RFC 4180 valid.

## Commit message template

```
feat(app): :export csv|json|insert <path> writes the visible result

Plan 06-09 added tab-completion for :export <fmt> <path> but the
writer behind it didn't exist — pressing Enter just produced
"unknown command". Fill the gap so a query result can actually
leave narwhal.

Three formats:

- csv     RFC 4180: header row, double-quote cells containing
          comma/quote/newline, double the quote to escape, NULL
          becomes the empty field, CRLF line ending.
- json    Array of objects; NULL stays null, numbers stay
          numeric. Value::Bytes that isn't valid UTF-8 serialises
          as {"$bytes": "<base64>"} so the round-trip survives.
- insert  One INSERT INTO <table> (cols) VALUES (...); per row.
          Requires the result to carry a known source table; if
          the dispatched SQL wasn't a single-table SELECT, the
          command sets a status message and writes nothing.

Streams via BufWriter<File> so a 10M-row export doesn't sit in
memory.  Honours an active 06-04 filter / sort — the exported
rows are exactly the *visible* rows when :export is invoked.

source_table tracking is a regex-based heuristic on the
dispatched SQL rather than a real parser, with the INSERT branch
rejecting when the heuristic fails.  Six new tests cover the
csv round-trip, NULL handling, json valid+invalid-UTF8 paths,
INSERT round-trip, and the missing-source error path.
```
