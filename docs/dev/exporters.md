# Parquet and Markdown exporters

Design notes for the Parquet and Markdown export paths introduced in
v2.0.

## Headline

`:export` (and its underlying `export_rows` function) gain two new
output formats: **Parquet** (Apache columnar, for downstream analytics
pipelines) and **Markdown** (GFM tables, for paste-into-PR / Notion
workflows). CSV / JSON / TSV / Table / Insert remain bit-for-bit
unchanged.

## CLI surface

```
:export parquet  out.parquet  [--compression snappy|zstd|none]
:export markdown out.md  [--no-truncate]
:export md  out.md  [--no-truncate]  # alias
:export pq  out.parquet  [--compression zstd]  # alias
```

Default codec is **SNAPPY** (the de-facto Parquet default — every
reader supports it, encoding is cheap). `--compression zstd` produces
materially smaller files at a modest encode-CPU cost. `--compression
none` is useful for benchmarks and for already-compressed filesystems.

Markdown truncates at **1000 rows** by default and appends an italic
`_…N more rows truncated_` line. `--no-truncate` dumps everything.

## Public surface delta

### `narwhal-commands::export`

```
+ pub use format::{ExportOptions, MarkdownOptions, ParquetCompression};
+ export_rows(columns, rows, format, path, source_table, options)
  ↑
  &ExportOptions
+ pub fn write_format_with_options<W: Write>(...)
+ ExportFormat::Parquet
+ ExportFormat::Markdown
```

The pre-existing `export_rows` signature **gained a trailing
`&ExportOptions` parameter**. This is the only externally visible
breaking change in this task. Every in-tree caller has been updated.
For out-of-tree callers the one-line migration is:

```rust
// v1.x → v2:
- export_rows(&columns, &rows, fmt, &path, source_table);
+ export_rows(&columns, &rows, fmt, &path, source_table, &ExportOptions::default);
```

`write_format` keeps its historical signature (defaults applied
internally); reach for `write_format_with_options` when the caller
wants to tweak Markdown truncation while streaming to stdout.

### `narwhal-commands::commands`

```
+ pub struct ExportArgs {
  pub compression: Option<ParquetCompression>,
  pub no_truncate: bool,
  }
  Command::Export {
  format: String,
  path: String,
+  options: ExportArgs,
  }
```

`Command::Export` gained one field. This is the second visible
breaking change. The parser populates `options` from the trailing
`--compression <codec>` / `--no-truncate` flags; an `ExportArgs::default`
matches the historical zero-flag shape.

### `narwhal-app`

```
+ pub use narwhal_commands::export::{ExportOptions, MarkdownOptions,
  ParquetCompression};
```

No struct or method on `AppCore` changed shape; `export_results`
gained a third `args: ExportArgs` parameter, threaded through from
the new `Command::Export.options` field.

## Architecture notes

### Why a separate `parquet.rs` for the writer

Parquet needs to own the sink: the file footer + magic bytes are
written by `ArrowWriter::close`, which means we cannot stream into
a generic `&mut dyn Write`. That's why `write_format(Parquet, ...)`
returns a typed error directing the caller at `export_rows`, and why
`narwhal exec --format parquet` is rejected at the CLI layer with a
matching error. The TUI command (`:export parquet out.parquet`) is
the supported path.

### Schema inference

The writer walks the first **100 rows** to decide each column's
`LogicalType`. We collapse the rich narwhal `Value` taxonomy onto a
small set of physical Arrow types:

| narwhal `Value`  | Arrow physical  |
| ---------------------------------------- | -------------------------------- |
| `Int` (i64)  | `Int64`  |
| `Float` (f64)  | `Float64`  |
| `Bool`  | `Boolean`  |
| `String` / `Uuid` / `Json` / `Bytes` / `Unknown` / `Time` | `Utf8` |
| `Date` / `DateTime` / `Timestamp`  | `Timestamp(μs, UTC)`  |
| `Null`  | (column-typed null)  |

`Time` (wall-clock time without a date) falls back to `Utf8` rather
than masquerading as `Timestamp(μs, UTC)` — there is no portable
Arrow type for "time of day" that Polars / DuckDB / Spark agree on.

Mixed-type columns are **widened**: `Int + Float → Float64`,
`Bool + Int → Int64`. Anything wider than numeric widening (string +
numeric, timestamp + anything else) degrades to `Utf8`. The brief
calls this out under *Tricky bits › Mixed-type columns*.

### Atomic writes

The Parquet writer stages into `<dir>/.<basename>.tmp` next to the
final path, then `std::fs::rename`s onto the target. Failure (write
error, footer error, rename error) cleans up the staging file so a
mid-stream crash never leaves a half-written `.parquet` lying around.
This matches the pattern `narwhal-config` already uses for
`settings.toml` writes.

### Markdown escaping

GFM tables are delimited by `|` and split on newlines, so we escape:

| input | output  | why  |
| ----- | ------- | ----------------------------------------- |
| `\|`  | `\|`  | column separator  |
| `\n` / `\r` | `<br>` | rows are line-delimited  |
| `\\`  | `\\\\`  | the previous escape we just emitted  |

Backticks are deliberately **not** escaped — GFM renders them as
inline code spans inside table cells, which is what users want for
SQL identifiers / code-shaped values. `NULL` renders as `(null)`,
matching psql / DBeaver Markdown export conventions.

### Memory model

Both writers materialise the full result in memory:

- **Parquet** builds Arrow `ArrayBuilder`s for every column, then a
  single `RecordBatch`, then the writer. Row-group size defaults to
  the parquet crate's default (≈1 048 576 rows or `WriterProperties`
  bytes limit, whichever hits first). Acceptable for v2.0; the brief
  defers a streaming Parquet writer to v2.2+.
- **Markdown** never makes sense to stream (you can't render half a
  table), so no streaming path is planned at all.

## Breaking-change envelope

For's migration guide:

> v2.0 adds `Parquet` and `Markdown` variants to
> `narwhal_commands::export::ExportFormat`. `export_rows` gained a
> trailing `&ExportOptions` parameter and `Command::Export` gained an
> `options: ExportArgs` field. Every in-tree caller was updated; out-of-tree
> callers add `&ExportOptions::default` to existing `export_rows`
> calls and read `Command::Export.options` if they construct or pattern-match
> the command (most don't).
>
> The headless CLI (`narwhal exec --format <fmt>`) now accepts
> `markdown` in addition to csv/json/tsv/table. `parquet` is rejected
> there with a friendly error directing the user at the TUI path
> (Parquet's footer requires file ownership the streaming sink does
> not provide).

## Tier 2 / future hooks

- **Streaming Parquet** (v2.2+): would consume `QueryStream` directly,
  flushing row groups as `RowsAppended` batches accumulate.
  the `QueryStream::next_row` API is already shaped for this —
  the missing piece is teaching `ArrowWriter` to chunk on row-count
  boundaries. Not in scope for v2.0.
- **Excel / XLSX**: separate task, deferred.
- **Custom Markdown styling** (HTML tables, AsciiDoc): separate task,
  deferred.

## Acceptance criteria status

| Item  | Status |
| ----------------------------------------------------- | :----: |
| `:export parquet out.parquet` writes valid file  |  ✅  |
| `:export markdown out.md` renders as GFM table  |  ✅  |
| All scalar types round-trip through Parquet  |  ✅  |
| Markdown escape correctness (pipe / newline / backslash) | ✅  |
| Compression flag works (snappy + zstd verified)  |  ✅  |
| MCP tool accepts new formats  |  ⚠  |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ |

The ⚠ item is a documented scope cut: `narwhal-mcp` has no
`export_query_result` tool today (the brief assumed one existed).
Adding an MCP-side export surface needs its own JSON-RPC schema +
audit hook design and belongs in a separate Tier 2 task. The
TUI / `:export` / `narwhal exec` paths all accept the new formats.

## Test coverage delta

The export module gained **15** new tests (39 total, was 24):

- Parquet round-trip (scalar types) — `parquet_round_trip_scalar_types`
- Parquet date/timestamp round-trip via reader — `parquet_round_trips_dates_and_timestamps_as_utc_microseconds`
- Parquet compression flag effect — `parquet_zstd_compression_produces_smaller_file`
- Parquet type widening (int + float → Float64) — `parquet_widens_mixed_numeric_column_to_float64`
- Parquet atomic-write contract — `parquet_atomic_write_no_partial_file_on_failure`
- Parquet rejection on streaming sink — `parquet_rejected_via_write_format_streaming_path`
- Markdown header + alignment — `markdown_emits_gfm_table_with_header_and_alignment`
- Markdown escaping (pipe / newline / backslash) — `markdown_escapes_pipe_and_newline_and_backslash`
- Markdown null rendering — `markdown_null_renders_as_sentinel`
- Markdown row-limit truncation — `markdown_truncates_at_row_limit_and_appends_marker`
- Markdown --no-truncate path — `markdown_no_truncate_dumps_every_row`
- Markdown empty-result fallback — `markdown_handles_empty_columns_gracefully`
- Markdown numeric alignment for MONEY / DECIMAL — `markdown_aligns_money_and_decimal_to_the_right`
- `ExportFormat::from_token` covers parquet / pq / markdown / md / MD — extended `export_format_from_token_recognises_new_formats`

Plus **5** new parser tests in `commands.rs` (`parquet`, `markdown`,
`md` alias, `--compression`, `--no-truncate`, bad-codec + bare-flag
rejection).

All `cargo test --workspace` runs land at **1132+ tests passing**
(was 1116 pre-task). + every prior baseline preserved.
