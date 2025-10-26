# Plan 01 — ClickHouse: Remove the per-call `Mutex<SharedState>`

## Why

`crates/narwhal-driver-clickhouse/src/lib.rs` currently holds
`Arc<Mutex<SharedState>>`. Every `http_query`, `stream`, `execute`,
`list_*`, `describe_*` call grabs the mutex for the entire duration of
the HTTP request. That serialises all concurrent calls on a single
connection — which is exactly the opposite of what an async HTTP client
gives you. `reqwest::Client` is already `Send + Sync` and uses an
internal connection pool, so the mutex is pure overhead.

## Constraints

- Behaviour-preserving. No new tests required, but the existing 26
  unit tests must keep passing.
- `clippy --all-targets -- -D warnings` clean, `fmt --check` clean.
- AGENTS.md rules: no `unwrap()`/`expect()` in production paths.
- One commit, conventional commit message, long-form body explaining
  the why and the change.

## Concrete steps

1. Change the connection field to `Arc<SharedState>`:
   ```rust
   pub struct ClickhouseConnection {
       inner: Arc<SharedState>,
   }
   ```
2. Drop the `tokio::sync::Mutex` import where no longer used; keep
   `mpsc` (still used by the stream).
3. Every call site that does `let state = self.inner.lock().await;`
   becomes `let state = &self.inner;` (or just inline-reference where
   no name is needed).
4. The streaming task in `Connection::stream` already clones the
   `Arc` for its spawned future; nothing changes there beyond the
   type.

## Files

- `crates/narwhal-driver-clickhouse/src/lib.rs` (only file touched)

## Acceptance

- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` reports 193 passed.
- `git diff --stat` is small (a dozen-or-so line changes), one file.

## Commit message template

```
refactor(driver-clickhouse): replace Mutex<SharedState> with Arc<SharedState>

reqwest::Client is already Send + Sync with an internal connection
pool, so the per-call mutex on SharedState just serialised work
that the HTTP client could otherwise parallelise across the pool.
Drop the Mutex; pass the SharedState behind a plain Arc so the
streaming task can still clone it cheaply for its spawned future.

No behaviour change, no new tests; the 26 existing unit tests
exercise the same paths and keep passing.
```
