# Code style

The workspace passes `cargo clippy -- -D warnings` under
`clippy::pedantic` + `clippy::nursery` with a documented allow-list
in the workspace `Cargo.toml`.

## Rust

- `?` for error propagation. `unwrap()` and `expect()` are
  forbidden outside tests.
- Errors: `thiserror` for library crates, `anyhow` for the binary
  crate and tests.
- Logging: `tracing`. `println!` and `eprintln!` are reserved for
  the binary's CLI output paths and `--help`.
- No `unsafe`. The workspace sets `unsafe_code = "deny"` and every
  library crate adds `#![forbid(unsafe_code)]` at the root.
- Avoid `.clone()` unless ownership transfer is what the call site
  actually wants.
- `#[non_exhaustive]` on every public struct or enum that may grow
  fields or variants in a future minor release.

## Async

- Prefer native `async fn` in traits (RPITIT).
- `#[async_trait]` only when the trait is used as
  `Box<dyn Trait>` and the runtime can't synthesize a vtable
  (see `dev/async-trait-style.md`).
- Channels: `tokio::sync::mpsc` for fan-in, `broadcast` for
  fan-out, `oneshot` for request / response.

## Modules

- One file per public type when the file exceeds ~400 lines.
- Module docs (`//!`) explain *why*, not *what*. Re-state
  invariants the type system can't.
- No banner comments (`// =====`, `// -----`). They distract.

## Tests

- Unit tests live in `#[cfg(test)] mod tests` at the bottom of the
  file under test.
- Integration tests live in `tests/`. One file per scenario.
- Property tests via `proptest` for parser and incremental-state
  surfaces.
- Avoid sleeping in tests. Use deterministic clocks
  (`tokio::time::pause`) or barriers.

## Formatting

- `cargo fmt --all` before every commit.
- Line length: 100. The `rustfmt.toml` enforces it.
- Imports grouped: `std`, external crates, internal crates,
  super / self. Sorted within each group.

## Commit messages

Conventional Commits:

```
feat(domain): EditorBuffer gains selection extension API
fix(drivers): MySQL VerifyCa now skips hostname check
refactor(app): split AppCore into core/state/
docs: scrub internal ticket codes from comments
chore: bump tracing-subscriber to 0.3.18
```

## See also

- [`build.md`](./build.md) — local CI invocation
- The workspace `Cargo.toml` — full lint configuration and
  allow-list rationale
