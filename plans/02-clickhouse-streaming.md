# Plan 02 — ClickHouse: real chunked streaming

## Why

The current `Connection::stream` in `narwhal-driver-clickhouse` calls
`response.bytes().await` and buffers the entire result body in memory
before parsing. Large analytical queries (millions of rows) blow up
RAM before the consumer sees a single row.

ClickHouse's HTTP API streams `TabSeparatedWithNamesAndTypes` line by
line; `reqwest::Response::bytes_stream()` exposes the chunks. The job
is to bridge the two so rows flow through the existing `mpsc` channel
as soon as their line is complete.

**Depends on Plan 01.** Apply 01 first.

## Constraints

- Behaviour-preserving: the existing TSV parsing logic (`parse_tsv_value`
  in `types.rs`) is unchanged. Only the *delivery* of bytes to the parser
  changes.
- Errors from the underlying stream must surface as `Error::Query`
  through `RowStream::next_row` — no panics, no silent drops.
- Backpressure: the `mpsc::channel(64)` already throttles producers;
  honour it (use `send().await`, not `try_send`).
- One commit. Conventional, long-form.
- `clippy --all-targets -- -D warnings` clean, `fmt --check` clean.
- AGENTS.md: no `unwrap`/`expect`.

## Concrete steps

1. Inside the `tokio::spawn` block in `Connection::stream`, replace
   `let body_bytes = response.bytes().await?;` plus the `String::from_utf8_lossy`
   single-buffer parser with a chunk-driven line buffer:

   ```rust
   use futures_util::StreamExt;
   let mut stream = response.bytes_stream();
   let mut buf: Vec<u8> = Vec::new();
   // … read header line + type line first, then forward rows …
   ```

2. Pull bytes until the first two newline-terminated lines are
   available (column names, then type strings). Send the
   `Vec<ColumnHeader>` through `header_tx` exactly once, then continue
   in row mode.

3. In row mode: for each newline-terminated line, parse with the
   existing `parse_tsv_value` helper per column, then
   `row_tx.send(Ok(CoreRow(...))).await`. If `send` returns `Err`,
   the consumer dropped the stream; break.

4. End-of-stream: when `stream.next().await` yields `None`, flush any
   incomplete trailing line (ClickHouse always terminates rows with
   `\n` so this is almost always empty; defensive handling for
   robustness).

5. Error path: on `stream.next().await` yielding `Some(Err(e))`,
   forward `Err(Error::Query(e.to_string()))` through `row_tx` and
   break.

6. Update the module-level doc comment in `lib.rs` so the "Streaming"
   section says **streamed**, not buffered. Mention `bytes_stream` by
   name so a future reader knows what the implementation choice is.

## Dependencies

Add `futures-util = "0.3"` to `crates/narwhal-driver-clickhouse/Cargo.toml`
(only the `StreamExt` adaptor is needed). Use `default-features = false`
plus `features = ["std"]` to keep the dep small.

## Files

- `crates/narwhal-driver-clickhouse/src/lib.rs`
- `crates/narwhal-driver-clickhouse/Cargo.toml`
- `Cargo.lock` (auto-updated)

## Tests

Add one new unit test exercising the line-buffered split logic
*outside* the HTTP path — pull the byte-feeding loop into a small
testable function that takes `impl Stream<Item = Result<Bytes, _>>`
and emits `(headers, types, rows)` via channels. The integration
tests behind `#[ignore]` continue to cover the real wire path.

Acceptance: total test count **194** (193 + 1 new).

## Acceptance

- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` reports 194 passed.
- Module-level doc no longer claims buffering — it claims streaming and
  is honest about it.

## Commit message template

```
feat(driver-clickhouse): real chunked streaming via bytes_stream

Connection::stream used to call response.bytes().await and buffer
the entire result body into RAM before parsing. Millions of rows
that ClickHouse is happy to dribble out one chunk at a time arrived
all at once on the consumer side, or — for results bigger than the
process's headroom — never at all.

Switch to reqwest::Response::bytes_stream() and feed a small line
buffer that walks the byte chunks, emits the two TSV header lines
once they're available, then forwards each completed row through
the existing mpsc channel as soon as the parser produces a Row.

Backpressure: the channel's bounded buffer (64) throttles the
spawned task; mpsc::send().await blocks producers when the consumer
is slow, which is exactly the shape we want.

One new unit test covers the byte-feeder in isolation by piping a
fake stream into the new chunk-driven decoder. Total test count
194.
```
