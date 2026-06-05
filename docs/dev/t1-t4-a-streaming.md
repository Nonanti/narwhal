# T1-T4-A — Streaming result pipeline

> Status: **landed on v2-dev**. Feeds T3-01 (migration guide) with the
> public-surface delta below.

## Headline

`narwhal_core::Connection` gains a new `query()` method that returns
a `QueryStream` — columns up-front, rows arriving asynchronously.
The TUI run worker (`narwhal_app::run::run_stream`) now drives results
through `Connection::query` and gains time-window batching so a slow
trickle still surfaces rows in the UI without waiting for the size
threshold.

## Public surface delta

### `narwhal-core`

```
+ pub mod query_stream;
+ pub use query_stream::QueryStream;
+ Connection::query(&mut self, sql: &str, params: &[Value])
      -> impl Future<Output = Result<QueryStream>> + Send;
+ DynConnection::query<'a>(&'a mut self, sql, params)
      -> BoxFuture<'a, Result<QueryStream>>;
```

`QueryStream` is a new public type. It is *not* `#[non_exhaustive]`
because every field is private; the constructor
(`QueryStream::new(columns, inner)`) is the only build path and is
non-breaking to extend.

`QueryStream` public methods:

| Method                           | Purpose                                     |
| -------------------------------- | ------------------------------------------- |
| `columns(&self) -> &[ColumnHeader]` | Schema available before the first row.   |
| `rows_yielded(&self) -> usize`   | Live counter used by the TUI title bar.    |
| `elapsed(&self) -> Duration`     | Live elapsed used by the TUI title bar.    |
| `next_row(&mut self) -> Option<Result<Row>>` | Fused row-at-a-time iterator. |
| `collect_all(self) -> Result<QueryResult>` | Drain into the materialised shape. |
| `collect_with_limit(self, limit) -> Result<(QueryResult, bool)>` | Bounded drain; `bool` = truncated. |
| `close(self) -> Result<()>`      | Awaitable cursor release.                  |

### `narwhal-config`

```
+ pub struct RunSettings {
      pub batch_size: usize,        // default 64
      pub stream_flush_ms: u64,     // default 50
  }
+ Settings::run: RunSettings  (v2 settings section)
+ pub use settings::RunSettings;
```

Both fields apply to `narwhal_app::run::run_stream` only. The MCP
`run_query` tool keeps its own row cap (`limit` parameter) and is
unaffected.

> A `stream_buffer` field (bounding the in-flight row channel
> between sync drivers and the async worker) was originally
> proposed but removed during T1-T4-A self-review: the driver-side
> wiring it would have controlled does not yet exist, so shipping
> the knob would have been a misleading public API. The struct is
> `#[non_exhaustive]` so the field can be re-added without breaking
> downstream consumers when the wiring lands.

### `narwhal-app`

```
+ pub struct StreamTuning { batch_size, flush_ms }
+ StreamTuning::new(batch_size, flush_ms) -> Self
+ StreamTuning::default() -> Self          // 64 / 50
+ RunContext::stream_tuning: StreamTuning
+ NextRowOutcome (private; row-loop control flow)
```

`AppCore::apply_settings` now reads `settings.run.batch_size` and
`settings.run.stream_flush_ms` and stores them on
`SessionState::stream_tuning`. Every dispatched `RunContext`
carries the latest tuning so `:reload-config` lands without a
restart.

The worker applies a defensive `batch_size.max(1)` floor locally so
a runtime mutation of the public `pub batch_size` field cannot
livelock the loop — belt-and-braces around
[`StreamTuning::new`]'s clamp.

## Breaking change envelope

For T3-01's migration guide:

> v2.0 introduces `narwhal_core::Connection::query` returning a
> `QueryStream`. The method has a default implementation built on
> top of `Connection::stream` so **existing driver implementations
> need not change**. External implementors that override every
> method (rare; only relevant for out-of-tree custom drivers) must
> either add a `query` override or rely on the default.
>
> The `DynConnection` sibling gained the same method, paid for by
> the existing blanket impl. No call-site change is required for
> consumers that hold `Box<dyn DynConnection>` / `Arc<dyn
> DynDatabaseDriver>`.
>
> A new `settings.run` section is recognised. v1 / unconfigured
> users get defaults that match v1.x worker behaviour
> (`batch_size = 64`, `stream_flush_ms = 50`).

### Migration recipe for out-of-tree `Connection` impls

```rust
// Before (v1.x):
impl Connection for MyDriver {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> { … }
    async fn stream(
        &mut self, sql: &str, params: &[Value],
    ) -> Result<Box<dyn narwhal_core::DynRowStream>> { … }
    // …
}

// v2.0: identical — Connection::query has a default body that wraps
// the existing `stream` impl in a `QueryStream`. Override only if
// the driver can produce the columns + cursor in a single round
// trip (sqlx native streams can).
```

## Tier 2 contract (chart / pivot)

T2-T4-C (inline ASCII chart) and T2-T4-D (pivot table) are the two
Tier 2 tasks that depend on this output. The contract they should
build against:

1. **Producer side**: dispatch is unchanged — the run worker still
   sends `RunUpdate::HeaderReady` and `RunUpdate::RowsAppended`
   batches. T2 widgets attach to those updates.
2. **Consumer-side ergonomics**: any new code path that wants the
   raw row stream goes through `Connection::query` (returns
   `QueryStream`). The chart / pivot pipeline should accept a
   `QueryStream` rather than a fully materialised `QueryResult` so
   it can aggregate incrementally — first-N-rows-render rather
   than wait-for-all.
3. **Memory bound**: T2 widgets should bound their own
   accumulator at `settings.run.stream_buffer` rows or at a
   widget-specific cap (e.g. pivot dimensions × measures). The
   worker is *not* responsible for capping consumer memory.
4. **Cancellation**: dropping the `QueryStream` releases the
   underlying cursor. T2 widgets that abandon a query (e.g. user
   navigates away from chart) should drop their stream handle.

## Cancellation semantics

- `Drop` on `QueryStream` runs synchronously and releases the
  wrapped `Box<dyn DynRowStream>`. The workspace drivers honour
  this by releasing their server-side cursor in their own `Drop`
  impls.
- `QueryStream::close()` is async and surfaces server-side release
  errors. Use it when the caller wants to flush a `PG portal
  close` / `MySQL KILL QUERY` / `ClickHouse HTTP body discard`
  round-trip before continuing.
- `QueryStream::collect_all` / `collect_with_limit` always invoke
  `close()` on the success path **and** on the error path, so
  callers never need to manually close after draining.

## Acceptance criteria status

| Item                                              | Status |
| ------------------------------------------------- | :----: |
| All drivers return `QueryStream`                  |   ✅   |
| `QueryStream::collect_all` round-trips tests      |   ✅   |
| TUI shows incremental row count                   |   ✅ (pre-existing via `streaming_counter.rs`) |
| Batching behaviour testable end-to-end            |   ✅ chunk-count assertions in `stream_tuning.rs` (M7 fixup) |
| First-row time on 1M-row query measurably faster  |   ⏳ benchmark deferred to integration pass |
| Memory bounded by a configurable knob             |   ⚠ `stream_buffer` field removed; re-add when the sync-driver mpsc seam is configurable end-to-end |
| Drop-mid-stream cancels query                     |   ⏳ requires live `pg_stat_activity` verification; sqlite/duckdb path is `mpsc::Receiver::recv` (cancellation-safe by tokio contract) |
| MCP behaviour unchanged                           |   ✅ MCP path not touched |
| Definition of Done passes                         |   ✅ (fmt, clippy -D warnings, rustdoc -D warnings, all tests dev+release) |

The ⏳ items need a real database environment to verify and are
tracked for the Tier 3 integration sweep. The ⚠ item is a
conscious scope cut documented above.
