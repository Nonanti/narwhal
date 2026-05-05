# Style Exceptions

Files that exceed the soft limits in [`STYLE.md`](STYLE.md). Each entry
is a deliberate, documented exception. Adding a new entry requires a
short rationale.

## File size > 500 LOC

Driver `mod.rs` files exceed 500 LOC; they are scheduled for a
submodule split (see roadmap Faz 1 Madde 4). Until that lands they
remain documented exceptions.

| File                                             | LOC   | Reason |
|--------------------------------------------------|-------|--------|
| `crates/narwhal-drivers/src/mssql/mod.rs`        | 1877  | Tiberius binding + value codec + streaming. Scheduled split. |
| `crates/narwhal-drivers/src/clickhouse/mod.rs`   | 1758  | HTTP transport + TSV streaming + type lattice. Scheduled split. |
| `crates/narwhal-drivers/src/duckdb/mod.rs`       | 1429  | Embedded engine with rich type lattice. Scheduled split. |
| `crates/narwhal-drivers/src/mysql/mod.rs`        | 1137  | `mysql_async` binding + value codec. Scheduled split. |
| `crates/narwhal-plugin-lua/src/lib.rs`           | 1106  | Lua FFI wiring lives in one place by convention; splitting interferes with `mlua` lifetime gymnastics. |
| `crates/narwhal-drivers/src/postgres/mod.rs`     | 1042  | `tokio-postgres` binding + value codec. Reference layout for the upcoming split (already has `ddl.rs`, `tls.rs`, `types.rs`). |
| `crates/narwhal-drivers/src/sqlite/mod.rs`       |  970  | `rusqlite` binding + value codec. Scheduled split. |
| `crates/narwhal-commands/src/commands.rs`        | 2045  | Command dispatch table. Scheduled split (one module per command). |
| `crates/narwhal-app/src/core/results_actions.rs` | 1030  | Action handlers over the result pane. Scheduled move to `narwhal-domain`. |
| `crates/narwhal-app/src/core/sessions.rs`        |  831  | Session state + IO. Scheduled state→domain, IO→app split. |
| `crates/narwhal-domain/src/editor.rs`            |  703  | Editor buffer + line cursor iterator. Single concept, kept together. |
| `crates/narwhal-drivers/src/clickhouse/types.rs` |  692  | TSV type parser. Internal helper used only by the driver. |
| `crates/narwhal-vim/src/machine.rs`              |  680  | The vim state machine itself; splitting would shred a single concept. |
| `crates/narwhal-commands/src/export/mod.rs`      |  604  | Dispatcher + tests. The actual format-specific writers live in sibling files. |
| `crates/narwhal-history/src/journal.rs`          |  598  | JSONL journal with redaction. Single responsibility. |
| `crates/narwhal-drivers/src/postgres/tls.rs`     |  560  | TLS config negotiation. |

## Clippy allow-list

Workspace allow-list lives in the root `Cargo.toml` under
`[workspace.lints.clippy]`:

- `module_name_repetitions` — narwhal-style names (`DriverRegistry`
  inside `narwhal-drivers::registry`) intentionally repeat.
- `must_use_candidate` — too noisy on builders and accessor methods.
- `missing_errors_doc` / `missing_panics_doc` — domain-level errors
  are documented at the `Error` enum, not on every fallible function.
- `similar_names` — vim's `Motion::WordForward` / `WordBackward` set
  is unavoidable.
- `cast_precision_loss` / `cast_possible_truncation` / `cast_sign_loss`
  — `usize ↔ u16` casts in TUI layout code are bounded by screen size.

## Resolved deferred items

The following splits were deferred in the original refactor and have
since been completed:

- `narwhal-app/src/core/mod.rs` (1498 → 150 LOC) — type definitions
  moved to `core/state/*`, the `impl AppCore` block split into
  `construct.rs`, `accessors.rs` and `dispatch.rs`.
- `narwhal-app/src/core/editor_dispatch.rs` (1066 LOC) → directory
  with `mod.rs`, `editor_keys.rs`, `search.rs`, `completion.rs`,
  `sidebar.rs`.
- `narwhal-commands/src/wizard.rs` (930 LOC) → directory with
  `mod.rs`, `fields.rs`, `state.rs`, `logic.rs`, `path.rs`.
