# Style Exceptions

Files that exceed the soft limits in [`STYLE.md`](STYLE.md). Each entry
is a deliberate, documented exception. Adding a new entry requires a
short rationale.

## File size > 500 LOC

| File                                            | LOC   | Reason |
|-------------------------------------------------|-------|--------|
| `crates/narwhal-driver-clickhouse/src/lib.rs`   | 1625  | Single-file driver; the TSV streaming, type lattice and HTTP transport are tightly coupled. Splitting buys little. |
| `crates/narwhal-app/src/core/mod.rs`            | 1498  | `AppCore` god struct + its setters. Slated for incremental strip in a future refactor pass. |
| `crates/narwhal-driver-duckdb/src/lib.rs`       | 1280  | Same shape as the ClickHouse driver — embedded engine with a rich type lattice. |
| `crates/narwhal-plugin-lua/src/lib.rs`          | 1106  | Lua FFI wiring lives in one place by convention; splitting interferes with `mlua` lifetime gymnastics. |
| `crates/narwhal-app/src/core/editor_dispatch.rs`| 1066  | Editor key/event dispatch over `AppCore`. Slated for split alongside the `AppCore` strip. |
| `crates/narwhal-driver-postgres/src/lib.rs`     | 1018  | `tokio-postgres` binding + value codec. |
| `crates/narwhal-driver-mysql/src/lib.rs`        |  960  | Similar shape to other driver `lib.rs` files. |
| `crates/narwhal-commands/src/wizard.rs`         |  930  | Step-state machine. Splitting per step is on the to-do list. |
| `crates/narwhal-driver-sqlite/src/lib.rs`       |  848  | `rusqlite` binding + value codec. |
| `crates/narwhal-commands/src/commands.rs`       |  754  | Command dispatch table. Each command is small; the file is mostly a switch. |
| `crates/narwhal-app/src/core/results_actions.rs`|  730  | Action handlers over the result pane. Splits naturally per action group; deferred. |
| `crates/narwhal-domain/src/editor.rs`           |  702  | Editor buffer + line cursor iterator. Single concept, kept together. |
| `crates/narwhal-driver-clickhouse/src/types.rs` |  694  | TSV type parser. Internal helper used only by the driver. |
| `crates/narwhal-vim/src/machine.rs`             |  680  | The vim state machine itself; splitting would shred a single concept. |

## Clippy allow-list

Workspace allow-list lives in the root `Cargo.toml` under
`[workspace.lints.clippy]`:

- `module_name_repetitions` — narwhal-style names (`DriverRegistry` in
  `narwhal-driver-registry`) intentionally repeat.
- `must_use_candidate` — too noisy on builders and accessor methods.
- `missing_errors_doc` / `missing_panics_doc` — domain-level errors
  are documented at the `Error` enum, not on every fallible function.
- `similar_names` — vim's `Motion::WordForward` / `WordBackward` set
  is unavoidable.
- `cast_precision_loss` / `cast_possible_truncation` / `cast_sign_loss`
  — `usize ↔ u16` casts in TUI layout code are bounded by screen size.
