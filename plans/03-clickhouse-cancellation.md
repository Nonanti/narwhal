# Plan 03 — ClickHouse: real query cancellation

## Why

Today the driver hard-codes `with_cancellation(false)` and returns
`None` from `cancel_handle()`. ClickHouse supports cancellation through
the HTTP `query_id` parameter + a second `KILL QUERY WHERE query_id =
'…'` request; the missing piece is tracking the active query's id on
the connection so the cancel handle has something to target.

**Depends on Plans 01 and 02.** Apply those first.

## Constraints

- Cancel must be best-effort but **honest**: if there's no active
  query, `cancel().await` returns `Ok(())` without issuing a no-op
  KILL.
- Concurrent in-flight queries on the same connection are allowed
  (pool already serialises per the connection, but the API doesn't
  promise mutual exclusion). Track query ids in a way that survives
  multiple concurrent calls — `Arc<Mutex<HashSet<String>>>` or
  similar.
- One commit, conventional, long-form.
- `clippy --all-targets -- -D warnings` clean, `fmt --check` clean.
- AGENTS.md: no `unwrap`/`expect`.

## Concrete steps

1. Add an `active_queries: Arc<Mutex<HashSet<String>>>` field to
   `SharedState` (or to `ClickhouseConnection` directly). Use the
   `tokio::sync::Mutex` because we'll hold it briefly across `.await`
   in `cancel`.

2. In `http_query`, `query_tsv`, and the `stream` setup path, generate
   a `query_id` (uuid v4), append it to the URL as `?query_id=…`,
   insert it into the set before sending, and remove it from the set
   when the request completes (use a guard with `Drop` semantics or
   the existing tokio task layout — whichever stays cleaner).

3. Implement `cancel_handle()` to return
   `Some(Box::new(ClickhouseCancel { … }))` again. `ClickhouseCancel`
   holds the same `Arc<SharedState>` and the same active-queries
   handle.

4. `CancelHandle::cancel()` reads the current set under the mutex,
   issues one `POST /?query=KILL QUERY WHERE query_id IN (...)`
   request with every active id, and returns `Ok(())` regardless of
   the server's response — the goal is best-effort interruption, not
   guarantees.

5. Flip `Capabilities::with_cancellation(true)` and update the
   module-level doc comment so the cancellation section is no longer
   labelled "MVP / not wired up".

6. Strip the `cancel_handle()` "see capabilities" comment.

## Files

- `crates/narwhal-driver-clickhouse/src/lib.rs`

## Tests

Add two unit tests that exercise the active-query tracking without a
real HTTP server:

1. `tracks_active_query_id`: drive the registration/deregistration
   path through a helper and assert the set is correctly drained.
2. `cancel_with_no_active_queries_is_noop`: build a connection,
   immediately call cancel, assert it returns `Ok(())` and no HTTP
   request was attempted.

(If the second test requires injecting an HTTP mock to assert "no
request" cleanly, simpler is fine — assert the active-queries set is
empty and skip the HTTP assertion.)

Acceptance: total test count **196** (194 + 2 new).

## Acceptance

- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` reports 196 passed.
- Module doc: cancellation section now describes what's implemented,
  not what's missing.
- `Capabilities::with_cancellation(true)`.

## Commit message template

```
feat(driver-clickhouse): wire up real query cancellation

ClickHouse cancellation is two requests: one tags the running query
with a query_id (already supported on the URL); the second issues
KILL QUERY WHERE query_id = '…' to the same server. The piece this
driver was missing was tracking the active query_id on the
connection so the cancel handle had something to target.

Add Arc<Mutex<HashSet<String>>> on SharedState, insert on every
outgoing request, remove on completion, and have CancelHandle::cancel
read the current set and fire a single KILL QUERY with the union.
Best-effort: server failure is ignored, an empty set is a no-op.

Flip with_cancellation(true) and update the module doc — the
"cancellation is not wired up" notice was misleading.

Two new unit tests cover the tracking logic without an HTTP server.
Total test count 196.
```
