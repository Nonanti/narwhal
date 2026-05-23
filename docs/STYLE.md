# Narwhal Code Style

Single source of truth for code quality. All crates must comply.

## Comments

- Comments explain **why**, never **what**. If the code needs a "what" comment, rename or simplify until it doesn't.
- No banner comments (`// ===== section =====`).
- No section dividers, no ASCII boxes, no decorative headers.
- No `// TODO: improve later`, `// for now`, `// helper`, `// elegant`, `// robust`, `// comprehensive`.
- No restating the function signature in a doc comment.
- Doc comments (`///`) are written for public API only, in English, one-line summary; add a paragraph only when the contract is non-obvious.
- `// FIXME:` and `// SAFETY:` are allowed and must be specific.

## Naming

- Crates: `narwhal-<area>`. Single noun, no abbreviations.
- Modules: snake_case nouns (`session`, not `sessions_handler`).
- Types: descriptive, no `Manager`, `Helper`, `Util`, `Handler` suffixes unless the type really is a handler trait.
- Files and modules describe **what they own**, not **what they do**.
- One concept per file. If a file holds two concepts, split it.

## Module layout

- `mod.rs` contains only `pub mod` and `pub use` re-exports. No logic.
- `lib.rs` exposes the public surface of the crate. No business logic.
- File length target: ≤ 500 LOC. Hard ceiling: 800. A file over 800 LOC must be split before merge.
- Function length target: ≤ 60 LOC. A function over 100 LOC must be split before merge.

## Errors

- Every crate defines its own `Error` enum with `thiserror`.
- Public API returns `Result<T, crate::Error>` aliased as `Result<T>`.
- Binaries use `anyhow` only at the top level (`main` / command entry).
- `unwrap`, `expect`, `panic!`, `unreachable!` are forbidden in non-test code. Replace with `?` or explicit error variants.
- `unsafe` is forbidden workspace-wide (`#![forbid(unsafe_code)]` in every crate root).

## Types

- `#[derive(Debug)]` on every public type.
- Derive `Clone` only when ownership requires it. Do not derive `Copy` on types with semantic identity.
- Prefer newtypes over raw `String`/`u64` for domain identifiers.
- No public fields on types that have invariants. Use constructors and accessors.
- Builders (`SomethingBuilder`) only when constructor argument count > 4 or has optional groups.

## Async

- `tokio` runtime, multi-threaded. No `block_on` inside async code.
- Domain crates are sync. Only IO crates (`pool`, `driver-*`, `mcp`) are async.
- No `std::sync::Mutex` across `await`. Use `tokio::sync::Mutex` or restructure.

## Logging

- `tracing` only. `println!`/`eprintln!` reserved for the binary's user-facing output (e.g. `--help`, CLI errors before the TUI starts).
- Spans for request-scoped work, structured fields over string interpolation.

## Lints

Every crate root:

```rust
#![forbid(unsafe_code)]
#![warn(clippy::all, clippy::pedantic, clippy::nursery)]
#![warn(missing_docs)]
#![allow(clippy::module_name_repetitions)]
```

CI gate:

```
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo test --workspace
```

`missing_docs` may be downgraded to `allow` on internal crates if the crate is `pub(crate)` only.

## Tests

- Unit tests live next to the code under `#[cfg(test)] mod tests`.
- Integration tests in `tests/` under the crate.
- `unwrap`/`expect` allowed in tests. Test names describe behaviour, not function names: `parses_quoted_identifier`, not `test_parse`.
- No sleeping in tests. Use channels or polling helpers.

## Dependencies

- Workspace-managed via `[workspace.dependencies]`. No version literals in member `Cargo.toml`.
- No new transitive dependency without grep-checking it's not already present under another name.
- Default features off on heavy crates; opt in explicitly.

## Commits

- Conventional commits: `feat:`, `fix:`, `refactor:`, `docs:`, `chore:`, `test:`, `perf:`.
- One conceptual change per commit. A crate extraction is a single commit.
- Subject ≤ 72 chars, imperative mood.
- Body explains rationale when the change is non-mechanical.

## Forbidden patterns

- `Arc<Mutex<HashMap<...>>>` exposed in a public API. Wrap it.
- God structs (> 15 fields). Split by responsibility.
- Cyclic dependency between modules. Extract the shared type.
- "Smart" macros that hide control flow.
- Re-exporting third-party types from the public API unless the type is part of the contract.
