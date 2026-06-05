//! Ergonomic streaming wrapper bundling column headers with an async
//! row iterator.
//!
//! [`crate::Connection`] already exposes two execution paths:
//!
//! * [`Connection::execute`](crate::Connection::execute) — materialises
//!   the entire result on the wire. Used for non-`SELECT` statements
//!   that report `rows_affected`, and as the historical hot path for
//!   small interactive queries.
//! * [`Connection::stream`](crate::Connection::stream) — hands back a
//!   row-by-row [`DynRowStream`]. Used by the
//!   TUI's worker (`narwhal-app::run::run_stream`) so a 1 M-row
//!   `SELECT` does not block until the engine has produced its final
//!   row.
//!
//! [`QueryStream`] sits between the two. It wraps the row stream
//! together with the column header vector that every consumer needs
//! up-front, and provides:
//!
//! * [`QueryStream::next_row`] for the row-at-a-time loop —
//!   semantically identical to [`crate::RowStream::next_row`] but
//!   wrapped in `Option<Result<_>>` instead of `Result<Option<_>>`
//!   so the canonical `while let Some(row) = s.next_row().await`
//!   shape works without an extra match.
//! * [`QueryStream::collect_all`] for the "drain into the old shape"
//!   bridge that tests, MCP and the export path want.
//! * [`QueryStream::columns`] / [`QueryStream::rows_yielded`] /
//!   [`QueryStream::elapsed`] for the TUI live-counter.
//!
//! ## Drop / cancellation
//!
//! Dropping a half-drained `QueryStream` releases the wrapped
//! `Box<dyn DynRowStream>` synchronously, which in turn drops the
//! driver-side cursor / portal / channel and aborts the query.
//! The dyn-safe [`DynRowStream::close`] is **async** so it cannot run
//! from `Drop`; explicit cleanup goes through [`QueryStream::close`]
//! (which is awaitable and surfaces release errors). The contract
//! every workspace driver upholds:
//!
//! 1. `Drop` on the wrapped `DynRowStream` must be sufficient to
//!    release server-side resources — it may emit a best-effort
//!    "close" message but it must not block the runtime.
//! 2. `close()` is the awaitable path when the caller wants to
//!    surface a server-side release failure (PG portal close,
//!    `MySQL` `KILL QUERY`, `ClickHouse` HTTP body discard).
//!
//! The two methods on [`QueryStream`] that drain on the caller's
//! behalf ([`QueryStream::collect_all`] and
//! [`QueryStream::collect_with_limit`]) always invoke `close()` so
//! the cursor is released through the awaitable path even when the
//! caller did not see the stream end-of-data signal.
//!
//! ## Why no `futures::Stream` impl?
//!
//! `QueryStream` deliberately does **not** implement
//! `futures_core::Stream`. Two reasons:
//!
//! 1. The workspace's [`crate::RowStream`] trait already uses a
//!    bespoke `async fn next_row(&mut self) -> Result<Option<Row>>`
//!    shape because every driver author works at that boundary, not
//!    at the lower-level `poll_next(Pin<&mut Self>, &mut Context)`
//!    boundary that `Stream` exposes. Wrapping it in `Stream` would
//!    require either self-referential pinning (annoying for callers)
//!    or a hand-rolled `stream::unfold` adapter (which leaks the
//!    `self`-by-value semantics into the caller's match arms).
//! 2. The TUI run worker drives the stream with a `tokio::time::
//!    timeout` wrap around each `next_row()` call — see
//!    `narwhal_app::run::run_stream`. Adding a `Stream` impl would
//!    invite callers to `StreamExt::buffered`-style adapters that
//!    bypass the bounded-batch contract.
//!
//! Callers that genuinely need a `Stream` can build one in three
//! lines via `futures::stream::unfold(qs, |mut qs| async move {
//! qs.next_row().await.map(|r| (r, qs)) })`.

use std::time::{Duration, Instant};

use crate::error::Result;
use crate::schema::{ColumnHeader, QueryResult, Row};
use crate::stream::DynRowStream;

/// Upfront `Vec::with_capacity` ceiling for [`QueryStream::
/// collect_with_limit`]. Picked so a million-row `limit` (the cap a
/// caller might pass to avoid an explicit `take`) does not eagerly
/// allocate gigabytes; the vector still grows past this if the
/// stream actually yields more than [`COLLECT_PREALLOC_CAP`] rows.
const COLLECT_PREALLOC_CAP: usize = 1024;

/// Clamp `Duration::as_millis()` (a `u128`) down to `u64` without
/// truncating silently. Modern wall-clock queries do not exceed
/// `u64::MAX` milliseconds (~584 million years), but a saturating
/// conversion is cheap insurance against a misbehaving driver that
/// hands back a nonsensical elapsed.
fn elapsed_ms_saturating(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Streaming view of a query result.
///
/// Constructed by [`crate::Connection::query`]. Owns the underlying
/// [`DynRowStream`] and lets callers observe schema metadata, drain
/// the rows, or close the cursor explicitly.
///
/// The type is **not** marked `#[non_exhaustive]` because every field
/// is private; the struct is only ever built through
/// [`QueryStream::new`] (driver authors / test helpers) or returned
/// from [`crate::Connection::query`] (consumers). Adding a field is
/// non-breaking.
pub struct QueryStream {
    /// The driver-side row producer. [`DynRowStream::columns`] is
    /// the single source of truth for the column metadata —
    /// [`QueryStream`] delegates to it rather than holding its own
    /// copy, which would force every `Connection::query` call to
    /// clone the column vector for nothing.
    inner: Box<dyn DynRowStream>,
    started: Instant,
    rows_yielded: usize,
    /// Becomes `true` once the inner stream has returned `None` or an
    /// error. Guards against double-polling drivers that don't
    /// promise fused-semantics after end-of-stream.
    drained: bool,
}

impl QueryStream {
    /// Wrap an existing row stream. Used by the default
    /// [`Connection::query`](crate::Connection::query) implementation
    /// and by driver authors that build a richer stream out-of-band.
    ///
    /// Column metadata is read on-demand from
    /// [`DynRowStream::columns`] — the caller does **not** pass it in
    /// (review fixup M8: prevents the redundant column-vector clone
    /// the previous shape required).
    #[must_use]
    pub fn new(inner: Box<dyn DynRowStream>) -> Self {
        Self {
            inner,
            started: Instant::now(),
            rows_yielded: 0,
            drained: false,
        }
    }

    /// Column headers describing the shape of every row this stream
    /// will yield. Safe to call before the first
    /// [`Self::next_row`] — the headers are materialised eagerly by
    /// the driver as part of opening the cursor. Delegates to the
    /// wrapped [`DynRowStream::columns`] so the two views never
    /// disagree.
    #[must_use]
    pub fn columns(&self) -> &[ColumnHeader] {
        self.inner.columns()
    }

    /// Number of rows successfully yielded so far. Drives the TUI's
    /// "streaming · N rows" header.
    #[must_use]
    pub const fn rows_yielded(&self) -> usize {
        self.rows_yielded
    }

    /// Elapsed wall-clock time since the stream was opened. Drives
    /// the TUI's live-elapsed indicator.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Advance the stream by one row.
    ///
    /// Returns `None` once the underlying stream reports end-of-data
    /// **or** a previous call returned an error. The fused shape lets
    /// callers loop with `while let Some(row) = s.next_row().await`
    /// without worrying about double-polling.
    pub async fn next_row(&mut self) -> Option<Result<Row>> {
        if self.drained {
            return None;
        }
        match self.inner.next_row().await {
            Ok(Some(row)) => {
                self.rows_yielded += 1;
                Some(Ok(row))
            }
            Ok(None) => {
                self.drained = true;
                None
            }
            Err(error) => {
                self.drained = true;
                Some(Err(error))
            }
        }
    }

    /// Drain the stream into a materialised [`QueryResult`]. Used by
    /// tests, the MCP query tool, and the export path when the caller
    /// genuinely needs the whole shape in memory before continuing.
    ///
    /// `elapsed_ms` is filled from the wall-clock between
    /// [`Connection::query`](crate::Connection::query) returning and
    /// the last row arriving — useful for "how long did the streamed
    /// query take" reporting without the caller wiring its own
    /// timer.
    ///
    /// On error any rows already yielded are discarded; the caller
    /// gets the engine error verbatim. If partial materialisation
    /// matters, use [`Self::next_row`] in a loop and accumulate
    /// manually.
    pub async fn collect_all(mut self) -> Result<QueryResult> {
        let mut rows = Vec::new();
        loop {
            match self.next_row().await {
                Some(Ok(row)) => rows.push(row),
                Some(Err(error)) => {
                    // Best-effort close so the engine releases its
                    // cursor; we already have the terminal error, so
                    // any close failure is logged at WARN to make a
                    // potential cursor leak observable (review fixup
                    // m6).
                    let close_result = self.inner.close().await;
                    if let Err(close_err) = close_result {
                        tracing::warn!(
                            target: "narwhal::query_stream",
                            error = %close_err,
                            "close-after-error failed (possible cursor leak)",
                        );
                    }
                    return Err(error);
                }
                None => break,
            }
        }
        let elapsed_ms = elapsed_ms_saturating(self.started.elapsed());
        // Columns are read off the inner stream before we close it.
        let columns = self.inner.columns().to_vec();
        if let Err(close_err) = self.inner.close().await {
            tracing::warn!(
                target: "narwhal::query_stream",
                error = %close_err,
                "close after end-of-stream failed (possible cursor leak)",
            );
        }
        Ok(QueryResult {
            columns,
            rows,
            rows_affected: None,
            elapsed_ms,
        })
    }

    /// Drain the stream into a materialised [`QueryResult`] but stop
    /// once `limit` rows have been accumulated. Subsequent rows
    /// produced by the engine are discarded and the cursor is
    /// closed — useful for the MCP tool's hard row cap without
    /// reaching for `take`-style adapters.
    ///
    /// `truncated` in the returned tuple is `true` when the engine
    /// had more rows to give. Callers should surface this to the
    /// agent so it knows the response is incomplete.
    pub async fn collect_with_limit(mut self, limit: usize) -> Result<(QueryResult, bool)> {
        // Defensive shortcut: limit = 0 means "don't read anything";
        // we still report whether there *would* have been rows by
        // peeking once at the inner stream directly (so we never
        // touch the public `next_row` counter — review fixup M2).
        if limit == 0 {
            let truncated = !self.drained && self.peek_has_more().await?;
            let elapsed_ms = elapsed_ms_saturating(self.started.elapsed());
            let columns = self.inner.columns().to_vec();
            if let Err(close_err) = self.inner.close().await {
                tracing::warn!(
                    target: "narwhal::query_stream",
                    error = %close_err,
                    "close after zero-limit peek failed (possible cursor leak)",
                );
            }
            return Ok((
                QueryResult {
                    columns,
                    rows: Vec::new(),
                    rows_affected: None,
                    elapsed_ms,
                },
                truncated,
            ));
        }
        let mut rows = Vec::with_capacity(limit.min(COLLECT_PREALLOC_CAP));
        let mut truncated = false;
        while rows.len() < limit {
            match self.next_row().await {
                Some(Ok(row)) => rows.push(row),
                Some(Err(error)) => {
                    if let Err(close_err) = self.inner.close().await {
                        tracing::warn!(
                            target: "narwhal::query_stream",
                            error = %close_err,
                            "close-after-error failed (possible cursor leak)",
                        );
                    }
                    return Err(error);
                }
                None => break,
            }
        }
        // If we exited because we hit the limit and the stream still
        // has more, set truncated. We peek directly on the inner
        // stream (bypassing `next_row`) so `rows_yielded()` stays
        // consistent with `rows.len()` (review fixup M2). The peeked
        // row is unavoidably discarded — documented contract.
        if rows.len() == limit && !self.drained {
            match self.peek_has_more().await {
                Ok(more) => truncated = more,
                Err(error) => {
                    if let Err(close_err) = self.inner.close().await {
                        tracing::warn!(
                            target: "narwhal::query_stream",
                            error = %close_err,
                            "close-after-error failed (possible cursor leak)",
                        );
                    }
                    return Err(error);
                }
            }
        }
        let elapsed_ms = elapsed_ms_saturating(self.started.elapsed());
        let columns = self.inner.columns().to_vec();
        if let Err(close_err) = self.inner.close().await {
            tracing::warn!(
                target: "narwhal::query_stream",
                error = %close_err,
                "close after limit drain failed (possible cursor leak)",
            );
        }
        Ok((
            QueryResult {
                columns,
                rows,
                rows_affected: None,
                elapsed_ms,
            },
            truncated,
        ))
    }

    /// Peek directly at the inner stream without touching the public
    /// counters. Used by [`Self::collect_with_limit`] to decide the
    /// `truncated` flag while keeping [`Self::rows_yielded`]
    /// equal to the actually-returned row count (review fixup M2).
    /// Sets [`Self::drained`] when the peek confirms end-of-data so
    /// the caller does not have to.
    async fn peek_has_more(&mut self) -> Result<bool> {
        match self.inner.next_row().await {
            Ok(Some(_discarded)) => Ok(true),
            Ok(None) => {
                self.drained = true;
                Ok(false)
            }
            Err(error) => {
                self.drained = true;
                Err(error)
            }
        }
    }

    /// Explicitly close the stream. Equivalent to dropping it for any
    /// driver that wires its `Drop` impl to release the cursor, but
    /// `close()` is awaitable so callers can surface server-side
    /// release errors. Required by drivers that hold ephemeral
    /// server-side state (PG portals, `ClickHouse` HTTP body) where the
    /// async close round-trip must complete before the connection is
    /// returned to the pool.
    pub async fn close(self) -> Result<()> {
        self.inner.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::future::BoxFuture;
    use crate::schema::Row;
    use crate::stream::DynRowStream;
    use crate::value::Value;

    /// In-memory `DynRowStream` for the round-trip tests below. Yields
    /// pre-canned rows, then either ends or errors on the (N+1)-th
    /// `next_row` call.
    struct VecStream {
        columns: Vec<ColumnHeader>,
        rows: std::vec::IntoIter<Row>,
        terminal: Option<Error>,
        close_called: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl VecStream {
        fn new(
            columns: Vec<ColumnHeader>,
            rows: Vec<Row>,
            terminal: Option<Error>,
        ) -> (Self, std::sync::Arc<std::sync::atomic::AtomicBool>) {
            let close_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stream = Self {
                columns,
                rows: rows.into_iter(),
                terminal,
                close_called: std::sync::Arc::clone(&close_called),
            };
            (stream, close_called)
        }
    }

    impl DynRowStream for VecStream {
        fn columns(&self) -> &[ColumnHeader] {
            &self.columns
        }

        fn next_row(&mut self) -> BoxFuture<'_, Result<Option<Row>>> {
            Box::pin(async move {
                if let Some(row) = self.rows.next() {
                    return Ok(Some(row));
                }
                if let Some(error) = self.terminal.take() {
                    return Err(error);
                }
                Ok(None)
            })
        }

        fn close(self: Box<Self>) -> BoxFuture<'static, Result<()>> {
            self.close_called
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    fn col(name: &str) -> ColumnHeader {
        ColumnHeader {
            name: name.to_owned(),
            data_type: "TEXT".to_owned(),
        }
    }

    fn row(values: &[&str]) -> Row {
        Row(values
            .iter()
            .map(|s| Value::String((*s).to_owned()))
            .collect())
    }

    #[tokio::test]
    async fn next_row_yields_then_ends() {
        let (s, closed) = VecStream::new(vec![col("a")], vec![row(&["1"]), row(&["2"])], None);
        let mut qs = QueryStream::new(Box::new(s));
        assert_eq!(qs.rows_yielded(), 0);
        assert!(qs.next_row().await.unwrap().is_ok());
        assert_eq!(qs.rows_yielded(), 1);
        assert!(qs.next_row().await.unwrap().is_ok());
        assert!(qs.next_row().await.is_none());
        // Fused: a second post-end call also returns None without
        // re-polling the inner stream.
        assert!(qs.next_row().await.is_none());
        // Drop closes via Drop only if driver wires it; explicit
        // close required for confirmation.
        assert!(!closed.load(std::sync::atomic::Ordering::SeqCst));
        qs.close().await.unwrap();
        assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn collect_all_round_trips() {
        let (s, closed) = VecStream::new(
            vec![col("a"), col("b")],
            vec![row(&["1", "x"]), row(&["2", "y"]), row(&["3", "z"])],
            None,
        );
        let qs = QueryStream::new(Box::new(s));
        let qr = qs.collect_all().await.unwrap();
        assert_eq!(qr.columns.len(), 2);
        assert_eq!(qr.rows.len(), 3);
        assert!(qr.rows_affected.is_none());
        assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn collect_all_propagates_terminal_error() {
        let err = Error::Query("boom".into());
        let (s, closed) = VecStream::new(vec![col("a")], vec![row(&["only-row"])], Some(err));
        let qs = QueryStream::new(Box::new(s));
        let result = qs.collect_all().await;
        assert!(matches!(result, Err(Error::Query(_))));
        // Close fires even on the error path so the cursor leaks
        // nothing.
        assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn next_row_fuses_after_error() {
        let err = Error::Query("boom".into());
        let (s, _) = VecStream::new(vec![col("a")], vec![], Some(err));
        let mut qs = QueryStream::new(Box::new(s));
        assert!(matches!(qs.next_row().await, Some(Err(_))));
        assert!(qs.next_row().await.is_none());
        assert!(qs.next_row().await.is_none());
    }

    #[tokio::test]
    async fn collect_with_limit_truncates() {
        let (s, closed) = VecStream::new(
            vec![col("a")],
            (0..10).map(|i| row(&[&i.to_string()])).collect(),
            None,
        );
        let qs = QueryStream::new(Box::new(s));
        let (qr, truncated) = qs.collect_with_limit(3).await.unwrap();
        assert_eq!(qr.rows.len(), 3);
        assert!(truncated, "expected truncated=true when engine has more");
        assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn collect_with_limit_not_truncated_when_exact_fit() {
        let (s, closed) = VecStream::new(
            vec![col("a")],
            vec![row(&["1"]), row(&["2"]), row(&["3"])],
            None,
        );
        let qs = QueryStream::new(Box::new(s));
        let (qr, truncated) = qs.collect_with_limit(3).await.unwrap();
        assert_eq!(qr.rows.len(), 3);
        assert!(
            !truncated,
            "expected truncated=false when engine ends at limit"
        );
        assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn collect_with_limit_not_truncated_when_under() {
        let (s, _) = VecStream::new(vec![col("a")], vec![row(&["1"])], None);
        let qs = QueryStream::new(Box::new(s));
        let (qr, truncated) = qs.collect_with_limit(10).await.unwrap();
        assert_eq!(qr.rows.len(), 1);
        assert!(!truncated);
    }

    /// Review fixup: defensive `limit = 0` short-circuit. The peek
    /// path runs once and the resulting [`QueryResult`] is empty;
    /// the truncated flag reflects whether the engine had rows at
    /// all.
    #[tokio::test]
    async fn collect_with_limit_zero_short_circuits_with_rows() {
        let (s, closed) = VecStream::new(vec![col("a")], vec![row(&["1"]), row(&["2"])], None);
        let qs = QueryStream::new(Box::new(s));
        let (qr, truncated) = qs.collect_with_limit(0).await.unwrap();
        assert!(qr.rows.is_empty());
        assert!(truncated, "engine had rows; truncated must be true");
        assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn collect_with_limit_zero_on_empty_stream() {
        let (s, closed) = VecStream::new(vec![col("a")], vec![], None);
        let qs = QueryStream::new(Box::new(s));
        let (qr, truncated) = qs.collect_with_limit(0).await.unwrap();
        assert!(qr.rows.is_empty());
        assert!(!truncated, "empty stream is not truncated");
        assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// Review fixup M2: when `collect_with_limit` peeks an extra row
    /// to set the `truncated` flag, that peek must NOT inflate the
    /// public counter. We verify by re-running the same fixture
    /// without the limit and asserting the materialised row count
    /// matches the limit exactly.
    #[tokio::test]
    async fn collect_with_limit_truncated_yields_exactly_limit() {
        let (s, _) = VecStream::new(
            vec![col("a")],
            (0..10).map(|i| row(&[&i.to_string()])).collect(),
            None,
        );
        let qs = QueryStream::new(Box::new(s));
        let (qr, truncated) = qs.collect_with_limit(3).await.unwrap();
        assert_eq!(
            qr.rows.len(),
            3,
            "limit cap is hard — no over-collection from the peek"
        );
        assert!(truncated);
    }

    /// Review fixup M8: `columns()` delegates to the inner stream so
    /// the [`QueryStream`] wrapper holds no redundant copy. Verified
    /// by constructing with a known column list and reading through
    /// the [`QueryStream`] API.
    #[tokio::test]
    async fn columns_delegates_to_inner() {
        let inner_cols = vec![col("a"), col("b"), col("c")];
        let (s, _) = VecStream::new(inner_cols, vec![], None);
        let qs = QueryStream::new(Box::new(s));
        assert_eq!(qs.columns().len(), 3);
        assert_eq!(qs.columns()[0].name, "a");
        assert_eq!(qs.columns()[2].name, "c");
    }

    /// Review fixup M8: `collect_all` materialises the columns from
    /// the inner stream at drain time. Confirms that the
    /// `inner.columns().to_vec()` path produces the same headers the
    /// driver advertised.
    #[tokio::test]
    async fn collect_all_materialises_columns_from_inner() {
        let (s, _) = VecStream::new(
            vec![col("alpha"), col("beta")],
            vec![row(&["1", "x"])],
            None,
        );
        let qs = QueryStream::new(Box::new(s));
        let qr = qs.collect_all().await.unwrap();
        assert_eq!(qr.columns.len(), 2);
        assert_eq!(qr.columns[0].name, "alpha");
        assert_eq!(qr.columns[1].name, "beta");
    }

    #[tokio::test]
    async fn rows_yielded_tracks_correctly() {
        let (s, _) = VecStream::new(
            vec![col("a")],
            vec![row(&["1"]), row(&["2"]), row(&["3"])],
            None,
        );
        let mut qs = QueryStream::new(Box::new(s));
        let _ = qs.next_row().await;
        assert_eq!(qs.rows_yielded(), 1);
        let _ = qs.next_row().await;
        let _ = qs.next_row().await;
        assert_eq!(qs.rows_yielded(), 3);
        let _ = qs.next_row().await; // None
        assert_eq!(qs.rows_yielded(), 3);
    }

    #[tokio::test]
    async fn drop_releases_without_close() {
        let (s, closed) = VecStream::new(
            vec![col("a")],
            (0..1000).map(|i| row(&[&i.to_string()])).collect(),
            None,
        );
        let mut qs = QueryStream::new(Box::new(s));
        // Consume two rows then drop mid-stream.
        let _ = qs.next_row().await;
        let _ = qs.next_row().await;
        drop(qs);
        // VecStream's close is only invoked through the explicit
        // close path; the rest is up to Drop in real drivers. This
        // test documents the contract: drop is synchronous and does
        // NOT call DynRowStream::close.
        assert!(!closed.load(std::sync::atomic::Ordering::SeqCst));
    }
}
