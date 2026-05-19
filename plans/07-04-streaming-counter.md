# Plan 07-04 — Streaming live row counter

## Why

F7 streams a query without showing any progress; for a 10-minute
million-row scan the user has no idea whether anything is
happening, whether the connection died, or how close to done
they are.

## Scope

The result pane title for a streaming query becomes:

```
streaming · 12.3k rows · 2.1s
```

Updated on every chunk arrival, throttled to ≤10Hz so the render
loop doesn't drown in updates when chunks come fast.

On stream completion the title flips to the normal `<rows> rows ·
<ms>` format.

On stream cancellation (F4) the title flips to
`cancelled at 12.3k rows · 2.1s`.

Format details:
- `12.3k` for ≥10,000; `1234` exact for <10,000; `1.2M` for ≥1M
- elapsed seconds with one decimal until 60s, then `MM:SS`

## Constraints

- AGENTS.md: no `unwrap()` / `expect()` in production code.
- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- One conventional commit, long-form.
- Throttle is **rendering**-side, not stream-side: every chunk
  still updates the counter; the redraw is debounced.

## Concrete steps

### Step 1: extend ResultState::Running

```rust
ResultState::Running {
    sql: String,
    index: usize,
    total: usize,
    columns: Vec<ColumnHeader>,
    rows: Vec<Row>,
    streaming: bool,
    started_at: Instant,        // NEW
    last_render: Instant,       // NEW (for throttle)
}
```

The `started_at` is captured when the stream task is spawned;
`last_render` starts equal to `started_at`.

### Step 2: chunk handler updates the count + throttles redraw

`handle_run_update` in `core.rs` already receives chunks via the
`RunUpdate` channel. The chunk branch:

```rust
RunUpdate::StreamChunk { rows: chunk, .. } => {
    if let ResultState::Running { rows, last_render, .. } = state {
        rows.extend(chunk);
        let now = Instant::now();
        if now.duration_since(*last_render) >= Duration::from_millis(100) {
            *last_render = now;
            // mark the frame as dirty so the next tick re-renders
            self.needs_redraw = true;
        }
    }
}
```

If `needs_redraw` doesn't exist yet, add a simple bool field
that the event loop checks; either way the throttle is the only
new logic.

### Step 3: format the title

`widgets/results.rs::render_results` builds the title from the
current state. Add helpers:

```rust
fn format_count(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_elapsed(d: Duration) -> String {
    let total = d.as_secs_f64();
    if total < 60.0 {
        format!("{total:.1}s")
    } else {
        let mins = (total / 60.0).floor() as u64;
        let secs = (total % 60.0).floor() as u64;
        format!("{mins:02}:{secs:02}")
    }
}
```

Title cases:
- `ResultState::Running { streaming: true, rows, started_at, .. }`
  → `streaming · {format_count(rows.len())} rows · {format_elapsed(started_at.elapsed())}`
- `ResultState::Rows { rows, elapsed_ms, streamed: true, .. }`
  → `{format_count(rows.len())} rows · {elapsed_ms}ms`
- `ResultState::Cancelled { rows_so_far, elapsed_ms }`
  → `cancelled at {format_count(rows_so_far)} rows · {elapsed_ms}ms`
  (this state may not exist yet — add it if needed for the
  cancel branch)

### Step 4: tests

`tests/streaming_counter.rs`:

1. `streaming_title_includes_rows_and_elapsed` — start a stream,
   push 100 chunks of 10 rows each, render, assert title
   contains "1000 rows" (or "1.0k rows") and a seconds value.
2. `throttle_prevents_redraw_storm` — push 100 chunks in <1ms,
   assert `needs_redraw` is set ≤2 times (the first chunk + at
   most one debounced follow-up within 100ms window).
3. `complete_flips_to_rows_count` — push chunks then send
   StreamComplete, assert title is "<N> rows · <ms>ms" not
   "streaming · ...".

Acceptance: +3 tests.

## Files

- `crates/narwhal-app/src/core.rs` (ResultState::Running fields,
  handle_run_update throttle, needs_redraw if needed)
- `crates/narwhal-tui/src/widgets/results.rs` (format helpers,
  title)
- `crates/narwhal-app/tests/streaming_counter.rs` (new)

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean
- `nix develop --command cargo test --all` reports +3 from baseline
- Manual smoke: F7 on a large table, watch the title tick.

## Commit message template

```
feat(results): live row counter for streaming queries

F7 streams a query without showing any progress; for a 10-minute
million-row scan the user has no idea whether anything is
happening, whether the connection died, or how close to done
they are.  Surface it in the result pane title:

  streaming · 12.3k rows · 2.1s

Updated on every chunk arrival, throttled to ≤10Hz on the render
side so a fast-arriving stream doesn't drown the redraw loop.
The counter formats with one-decimal SI suffixes — 1234 stays
exact, 12345 becomes 12.3k, 1234567 becomes 1.2M — and the
elapsed value flips from `2.1s` to `MM:SS` at the one-minute
mark.

On stream completion the title becomes `<rows> rows · <ms>ms`,
exactly matching the non-streaming dispatch path.  On
cancellation (F4) the title becomes `cancelled at <rows> rows ·
<ms>ms` so an interrupted scan still reports what it managed.

ResultState::Running gains started_at and last_render Instants;
handle_run_update bumps the row count on every chunk and only
sets needs_redraw when 100ms has elapsed since the last redraw —
the throttle is render-side, the count itself is exact.

Three new tests cover title formatting, throttle behaviour, and
the streaming-to-complete title transition.
```
