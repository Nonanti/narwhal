# Contributing to narwhal

Thanks for considering a contribution. This document covers the
workflow, the conventions we enforce, and the bar a change has to
clear before it can land on `main`.

## Getting started

```sh
git clone https://github.com/Nonanti/narwhal
cd narwhal
nix develop          # or: install rust 1.75+, cmake, libclang
cargo test --workspace
cargo run -- tui
```

DuckDB's bundled C++ tree needs `cmake` and `libclang`. On Debian/Ubuntu:
`sudo apt install cmake libclang-dev`.

## Workflow

1. **Open an issue first** for any non-trivial change. Saves both of
   us a round of "this overlaps with planned work".
2. Branch off `main`. Branch name: `feat/short-slug`, `fix/short-slug`,
   `refactor/short-slug`, `docs/short-slug`.
3. Commit in [Conventional Commits](https://www.conventionalcommits.org/)
   format. Examples:
   - `feat(driver-postgres): COPY FROM support`
   - `fix(mcp): reject backtick identifier bypass`
   - `refactor(app): extract ModalState from AppCore`
   - `docs(readme): clarify SSH tunnel setup`
4. Open a PR against `main`. Fill in the template. Link the issue.
5. CI must be green. Reviewer will look at: correctness, style
   compliance, test coverage, doc updates.

## What we expect from a PR

| Checklist | Why |
|-----------|-----|
| `cargo fmt --all` | Style consistency, enforced in CI |
| `cargo clippy --workspace --all-targets -- -D warnings` | Pedantic + nursery, no exceptions |
| `cargo test --workspace` green | 825+ tests, regressions block merge |
| New behaviour has a test | Headless TUI is testable, no excuse |
| CHANGELOG.md updated under `[Unreleased]` | Users find changes here |
| No `unwrap`/`expect`/`panic!`/`unreachable!` in prod code | Style rule, see docs/STYLE.md |
| `#![forbid(unsafe_code)]` preserved | Safety invariant, every crate |
| Public API changes documented with `///` | rustdoc builds with `-D warnings` |

## Code style

The full rulebook is in [`docs/STYLE.md`](docs/STYLE.md). Highlights:

- **No `println!`/`eprintln!`** in production code — use `tracing`.
  The only exception is pre-TUI CLI parse errors.
- **`thiserror` for libraries, `anyhow` at the binary top.**
- **File target ≤500 LOC, function target ≤60 LOC.** Documented
  exceptions live in [`docs/EXCEPTIONS.md`](docs/EXCEPTIONS.md).
- **"Why" comments only.** No banner comments, no restating what the
  code does.
- **tokio multi-threaded.** No `std::sync::Mutex` held across an
  `.await`. Use `parking_lot::Mutex` for synchronous critical
  sections, `tokio::sync::Mutex` only when the lock must cross await.
- **Workspace deps only.** Add new crates to the root `Cargo.toml`
  `[workspace.dependencies]` and reference them with `.workspace = true`.

## Architecture

Layer rule: arrows always point down.

```
narwhal (bin) -> narwhal-app -> narwhal-{tui, commands, mcp}
              -> narwhal-domain -> narwhal-core -> drivers, pool, config, ...
```

The view layer (`narwhal-tui`) reads domain state by reference and
never mutates it. Only `narwhal-app` mutates domain. If your PR
breaks this rule the reviewer will ask you to fix it.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full map.

## Adding a new database driver

1. Create `crates/narwhal-drivers/src/<name>/` mirroring the layout
   of an existing backend (sqlite is the smallest reference).
2. Implement `DatabaseDriver` + `Connection` from `narwhal-core`.
3. Add a cargo feature in `crates/narwhal-drivers/Cargo.toml` and
   surface it from the workspace root + `narwhal-mcp`.
4. Register it in `narwhal-drivers::registry::with_defaults` behind
   the feature.
5. Add a row to the capability table in the README.
6. Add at least one integration test under `#[ignore]` that exercises
   the real driver against a docker-compose'd instance.

## Plugin contributions

Lua plugins live in `examples/plugins/`. PRs welcome. Each plugin
should:

- Open with a short doc-comment explaining the use case.
- Use only the documented `narwhal.*` API.
- Handle the timeout budget gracefully (no infinite loops).
- Carry an entry in `examples/plugins/README.md`.

## Releasing

Maintainers only. See [`docs/RELEASING.md`](docs/RELEASING.md).

## Code of Conduct

Participation is governed by the
[Contributor Covenant](CODE_OF_CONDUCT.md). By contributing you
agree to abide by it.

## Licence

By submitting a contribution you agree to license your work under
the project's dual MIT OR Apache-2.0 licence.
