# Group 3: APP + TUI — Codebase Review

**Scope:** `narwhal-app`, `narwhal-tui`, `narwhal-vim`, `narwhal-diagram`, `narwhal-pivot`
**LOC:** ~22K | **Date:** 2026-06-05 | **Branch:** v2-dev (v2.0.0 released, clippy/fmt/test all clean)

---

## 1. Dispatch Reducer

### Verdict: CLEAN — single-entry, well-separated mutation → side-effect

`AppCore::dispatch` is effectively `handle_key` / `execute_command`, both funnelling through a single `&mut self` entry point. State mutation and side effects are well-separated:

- **Mutations** happen directly on `self.ui`, `self.session`, `self.process`.
- **Async side effects** (meta_channel, run_channel) go through `dispatch_meta()` / `spawn_run()`.
- `meta_channel` routes correctly: `handle_meta_update` does a session-id / tab-id staleness check before applying (H7/H8/C5 invariants).

**Notable pattern:** `run_tab_index()` resolves the stable tab index at run start so mid-run tab switches don't scribble into the wrong tab (K1-A fix). This is correct and well-documented.

### Finding [M1] — `handle_run_update` index-resolved each call

`crates/narwhal-app/src/core/run_loop.rs:20` — `let rt = self.run_tab_index().await;` is called per-variant inside the match arm. This means every `RunUpdate` re-resolves the tab index. If the tab were somehow closed between `StatementStarted` and `RowsAppended`, the index could shift. However, `run_tab_index` is documented to resolve via `process.run_tab` which is only cleared on `AllDone`, so this is safe. No action needed.

### Finding [I1] — `await_pending_session_opens_sync` still blocks the key handler

`crates/narwhal-app/src/core/dispatch.rs:45` / `crates/narwhal-app/src/core/run_loop.rs:162-175` — The H7 bridge `await_pending_session_opens_sync` (up to 5s timeout) is called at the top of `handle_key` when `pending_session_opens` is non-empty. The docstring acknowledges this and explains the trade-off (test backward compat). The timeout cap prevents deadlock. **Severity: INFO** — deferred cleanup, not a bug.

---

## 2. State Invariants

### Verdict: ROBUST — OOB clamped, stable tab IDs

- **Tab vector + active_tab**: Every mutation (`new_tab`, `close_tab`, `cycle_tab`) clamps `active_tab` to `tabs.len() - 1` after modification. `close_tab` at `crates/narwhal-app/src/core/tabs.rs:32` does `if self.ui.active_tab >= self.ui.tabs.len() { self.ui.active_tab = self.ui.tabs.len() - 1; }`.
- **Stable tab IDs**: `Tab::id` is a monotonically-assigned `u64` that survives index shuffling (C5 fix). Meta updates carry `tab_id` not `active_tab`.
- **Sidebar index**: `rebuild_sidebar` at `crates/narwhal-app/src/core/construct.rs:205` clamps `sidebar_index >= sidebar_items.len()`. The render path pre-clamps `sidebar_scroll` too.
- **ResultBundle.active**: `next()`/`prev()` guard with `states.len() > 1`; `click_result_tab` bounds-checks at `crates/narwhal-app/src/core/editor_dispatch/editor_keys.rs:78`.

### Finding [L1] — `swap_remove(0)` in `AllDone` shifts indices

`crates/narwhal-app/src/core/run_loop.rs:91-96` — During `AllDone`, two `swap_remove(0)` calls on `states` and `views` are performed to move the current active entry into `pending_result_entries_states/views`. `swap_remove(0)` on a vec swaps the last element into position 0, which changes the index of other elements. This is fine because both vectors are immediately consumed via `std::mem::take` right after, but the intermediate state has the last element at index 0. If there were any early return between the two `swap_remove` calls and the `std::mem::take`, the bundle would be in an inconsistent state. **Severity: LOW** — the current flow has no early returns between the operations, but a future edit could introduce one.

---

## 3. Async Hygiene

### Verdict: GOOD — fire-and-forget tokio::spawn with bounded channels

- **Run updates**: `spawn_run` ships through a bounded `mpsc` channel (capacity 128). The sender is cloned into the worker; the receiver is drained on the event loop.
- **Meta updates**: Same pattern, same capacity.
- **Audit emit**: `tokio::spawn` fire-and-forget with `Arc<AuditService>` clone. The bounded mpsc inside the audit service protects against slow sinks.
- **Plugin dispatch**: Runs inline on the event loop (documented as a test-compat trade-off, H7 follow-up).
- **Cancel**: `spawn_cancel` correctly takes the `CancellationToken` out of the slot under a short-lived guard, then drops the guard before awaiting `cancel()`. No lock-held-across-await.

### Finding [I2] — `JoinHandle` from `schedule_schema_refresh` not awaited

`crates/narwhal-app/src/core/sessions.rs:152-159` — The `tokio::spawn` in `schedule_schema_refresh` produces a `JoinHandle` that is converted to `AbortHandle` via `.abort_handle()`. The actual `JoinHandle` is dropped, meaning errors from the spawned task (e.g., `run_tx.send` failure) are silently swallowed. This is intentional — the task is best-effort and failure is logged via the `let _ = tx.send(...)` pattern — but worth noting. **Severity: INFO**.

### Finding [I3] — `describe_table_into_result` runs blocking describe on event loop

`crates/narwhal-app/src/core/editor_dispatch/sidebar.rs:201-230` — The sidebar Enter handler (`activate_sidebar_selection` → `describe_table_into_result`) calls `pool.acquire().await` and `conn.describe_table().await` inline on the event loop. The docstring acknowledges this as "Sprint 11 deferred" and notes the typical cost is <30ms. For large schemas this could freeze the UI. **Severity: LOW** — acceptable for v2.0 given the documented cost, but should move to the meta channel in a follow-up.

---

## 4. Locks

### Verdict: CORRECT — std::sync::Mutex for sync paths, no lock-across-await

- **`PluginConnectionState`**: `Arc<std::sync::Mutex<PluginConnectionState>>` in `deps.plugin_state`. Every access is a short-lived lock (clone the pool out, drop guard) and never spans an `.await`. `unwrap_or_else(PoisonError::into_inner)` is used for recovery, which is appropriate for a UI app where a panic in a previous lock holder shouldn't kill the session.
- **`ActiveCancel`**: `Arc<tokio::sync::Mutex<Option<CancellationToken>>>`. The `spawn_cancel` method correctly takes the handle out under a short-lived guard and drops it before awaiting `cancel()`.
- **`refresh_pending`**: `Arc<AtomicBool>` with `Ordering::Release`/`Acquire` — correct for the cross-task flag.

### Finding [M2] — Poison recovery in `close_session` vs `apply_opened_session`

`crates/narwhal-app/src/core/sessions.rs:103` and `:88` both use `unwrap_or_else(PoisonError::into_inner)` on `plugin_state.lock()`. This is fine for a single-threaded UI. However, if a panic occurs while holding the lock (e.g., in `state.pool = Some(session.pool.clone())`), the recovered state may be inconsistent. The alternative (propagating the error) would crash the UI, so the current choice is pragmatic. **Severity: INFO** — standard poison recovery pattern.

---

## 5. Multi-Cursor

### Verdict: CORRECT MVP — well-documented v2.1 scope boundaries

- **Sorted invariants**: `secondary_cursors` is a sorted `Vec<(usize, usize)>` with `binary_search` insertion and `dedup` after edits. Primary/secondary coincidence is filtered.
- **Insert propagation**: `raw_insert_char_multi` at `narwhal-domain/src/editor.rs:392-445` processes positions left-to-right with per-row shift tracking. Correct.
- **Multi-line paste**: `insert_str` collapses secondaries when text contains `\n`, with a sticky notification. Documented as MVP scope limitation.
- **Delete propagation**: `delete_prev_char_multi` handles multi-cursor backspace.

### Finding [M3] — Secondary cursors not re-sorted after insert_str single-char path

`crates/narwhal-domain/src/editor.rs:466-477` — When `insert_str` processes characters one-by-one and `secondary_cursors` is non-empty, each char goes through `raw_insert_char_multi`, which internally sorts and dedups the secondaries after each char. This means `N` chars × `O(M log M)` sorts per paste. For short pastes this is fine, but for a large single-line paste (no `\n`), the cost is `O(N * M log M)`. **Severity: LOW** — real-world pastes are typically short, and the multi-cursor set is small.

### Finding [L2] — No collision detection between secondaries after edit

After `raw_insert_char_multi`, secondaries are deduped against each other and against the primary. However, two secondaries on the same row that were originally separated by 1 character will merge into the same position after a `delete_char` at that position. The `delete_prev_char_multi` method handles this correctly by tracking shifts, but the `new_secondaries.dedup()` at the end of `raw_insert_char_multi` is a logical dedup, not a spatial one. If two secondaries end up at the same position, only one survives. This is the expected vim-like behavior. **Severity: INFO**.

---

## 6. Treesitter Cache

### Verdict: CORRECT — length-keyed invalidation with documented limitations

`crates/narwhal-app/src/core/state/tab.rs:90-122` — `sql_highlights()` recomputes when `sql_highlights_buf_len != source.len()`. The docstring at line 95 explicitly acknowledges the within-line same-length stale-span limitation and defers a precise revision counter to the multi-cursor task.

### Finding [I4] — `reparse` not fed incremental edit deltas

The `sql_highlights()` method calls `parser.reparse(&source)` which does a full reparse. The tree-sitter API supports incremental parsing via `Parser::parse_with` with an edit delta, which is much faster. The current code discards the old tree on each call. The docstring at line 102 acknowledges this: "a future follow-up; the editor doesn't surface byte-level edit events yet." **Severity: INFO** — performance opportunity for v2.1.

---

## 7. Plugin Executor

### Verdict: CLEAN — short lock, no await

`crates/narwhal-app/src/core/plugin_executor.rs:42-62` — The `SqlExecutor::run` method:
1. Takes the `Mutex` guard, clones pool + in_transaction flag, drops guard.
2. Checks `in_transaction` → returns error if true.
3. Acquires from pool, executes.

No lock-across-await. The `in_transaction` guard prevents silent correctness bugs where a plugin SQL wouldn't see uncommitted state.

### Finding [I5] — No timeout budget enforcement in executor

The `AppPluginExecutor` does not enforce a timeout on `conn.execute(sql, &[])`. The `PluginRegistry::dispatch` does enforce a timeout (visible in the `PluginError::Timeout` variant), but that's at the command-dispatch level, not at the `sql_run` level. A malicious or buggy plugin could issue a long-running query that blocks the executor indefinitely. **Severity: LOW** — the pool-level query timeout (if configured on the driver) provides a backstop, and the plugin command timeout covers the outer dispatch.

---

## 8. Audit Emit

### Verdict: CORRECT — paired open/close, ordering sound

- **ConnectionOpened/Closed**: `apply_opened_session` emits `ConnectionOpened` with a fresh `session_id`. `close_session` emits `ConnectionClosed` with `duration_ms` computed from `audit_session_started_at`. The `session_id` is cleared to `Uuid::nil()` on close. If `close_session` is called without an active session (the `.take()` returns `None`), no `ConnectionClosed` is emitted — correct.
- **PluginLoaded**: The audit emit is in `register_lua_plugin` at `crates/narwhal-app/src/core/plugins.rs:37-44`. The `auto_load_plugins` method calls `register_lua_plugin` for each discovered plugin, which emits the event. The audit service is installed before `auto_load_plugins` is called in the binary startup sequence, so ordering is correct.

### Finding [M4] — Audit `Configuration` change wording inconsistency

`crates/narwhal-app/src/core/sessions.rs:241-244` — `remove_connection` emits `"connection removed: <name>"`. `forget_password` emits `"credential forgotten: <name>"`. These use different wording conventions ("connection removed" vs "credential forgotten"). The `by` field is always `"cli"`. A SIEM consumer would need to parse the free-form `change` string. **Severity: LOW** — the `change` field is documented as free-form; structuring it is a v2.1 concern.

### Finding [L3] — `ConnectionClosed` not emitted on crash/panic

If the process crashes (OOM, panic), `close_session` is never called and the audit log shows an unpaired `ConnectionOpened`. This is inherent to fire-and-forget audit and acceptable for v2.0. A future improvement could flush on `Drop` or register a `ctrlc` handler. **Severity: INFO**.

---

## 9. Persist Hook

### Verdict: CLEAN — proper boundary, OOB clamp, atomic writes

- **Projection/restore**: Both `project_workspace` and `apply_workspace` are `pub(crate)` in `core/persist_hook.rs`. The `persist` module re-exports them as `snapshot` / `apply`, maintaining the private state boundary.
- **OOB clamp**: `restore_tabs` at `crates/narwhal-app/src/core/persist_hook.rs:83` clamps `active_tab` to `tabs.len().saturating_sub(1)`. `set_cursor` and `set_scroll` both clamp internally.
- **Atomic write**: `atomic_write` in `persist/paths.rs` writes to a temp file with `0o600` permissions and renames. The `LockGuard` RAII pattern prevents stale locks.
- **Per-pid fallback**: On lock contention, the snapshot is written to `workspace-state.${pid}.toml`. Stale locks older than 60s are reaped.

### Finding [L4] — Per-pid fallback files never cleaned up

When a narwhal instance writes to a per-pid fallback file, that file persists indefinitely. Subsequent clean exits that successfully acquire the canonical lock do not reap orphaned per-pid files. Over time with many contended exits, the config directory could accumulate stale `workspace-state.*.toml` files. **Severity: LOW** — cosmetic/cleanup concern, not a correctness issue.

---

## 10. TUI Render

### Verdict: CLEAN — immutable per-frame, stable theme cycle

- **RootLayout**: Built fresh each frame in `render()` at `crates/narwhal-app/src/core/dispatch.rs:43-100`. No mutation of the layout during render.
- **Theme cycle**: `apply_settings` at `crates/narwhal-app/src/core/construct.rs:155-170` maps `narwhal_config::Theme` variants to `Theme::DARK`/`LIGHT`/`HIGH_CONTRAST` const values. The `#[non_exhaustive]` fallback goes to `DARK`. Stable.
- **Chart/Pivot fallback**: `render_chart_placeholder` and `render_pivot_placeholder` render a dim paragraph when derivation fails. Both are exercised in tests.

### Finding [I6] — `render()` borrows `tab` mutably while reading `self.ui.active_tab`

`crates/narwhal-app/src/core/dispatch.rs:42-43` — `let pending_count = self.ui.tabs[self.ui.active_tab].pending.len();` is read *before* `let tab = &mut self.ui.tabs[self.ui.active_tab];` to avoid overlapping borrows. This is correct and the comment explains why. **Severity: INFO** — good defensive pattern, no issue.

---

## 11. Mouse / Key Chords

### Verdict: CLEAN — case-insensitive parse, modal-aware dispatch

- **KeyChord parse**: (Verified via memory `jncjd9w3rkzenz9kqo2h7bip`) Case-insensitive.
- **Modal dispatch**: `handle_key` at `crates/narwhal-app/src/core/dispatch.rs:99-186` walks modals in priority order: wizard → confirm → json_viewer → diagram → pending_preview → help → history → snippets → goto → global_key → editor/sidebar/results. Each modal intercepts and consumes keys that it handles.
- **Conflicting bindings**: `Ctrl-N` is deferred to completion popup when the popup is open and editor is focused (`crates/narwhal-app/src/core/editor_dispatch/mod.rs:82-90`). `Ctrl-S` in editor triggers stream; in results it's reserved for pending commit. This separation is well-documented.

### Finding [L5] — `handle_scroll` uses `apply_motion` for editor scroll

`crates/narwhal-app/src/core/dispatch.rs:223-230` — Mouse wheel scroll over the editor uses `DomainMotion::Down`/`Up` with count 1, which moves the *cursor* one line, not the viewport. This differs from the typical terminal scroll behavior where the viewport moves while the cursor stays on the same line. The `ensure_visible` call afterward re-centers the viewport on the cursor, so the net effect is that both cursor and viewport move together. **Severity: LOW** — acceptable UX for v2.0, but a true viewport-scroll-without-cursor-move would be more conventional.

---

## 12. Vim Mode

### Verdict: CORRECT — clean state machine, good test coverage

- **Motions**: All basic motions (h/j/k/l/w/b/0/$/G) are mapped. `domain_motion` translates `VimMotion` → `DomainMotion` isomorphically.
- **Count prefix**: `pending_count` accumulates digits, clamps to `MAX_COUNT` (999,999), resets after motion. `0` is treated as a motion when there's no pending count (vim-correct).
- **Operator-pending**: `d`/`y`/`c` enter `OperatorPending` mode. Doubled operators (`dd`/`yy`/`cc`) apply line-wise. Escape cancels. Unknown keys cancel and return to Normal.
- **Visual mode**: `v`/`V` enter Visual/VisualLine. Operators in visual mode apply to the selection and return to Normal (or Insert for `c`).

### Finding [M5] — `gg` (go to first line) not implemented

Normal mode `G` goes to the last line (`Motion::FileEnd`), but there's no `gg` mapping for `Motion::FileStart`. `FileStart` exists as a `Motion` variant but is never produced by the state machine. Pressing `g` in normal mode does nothing (falls through to `_ => Action::Pending`). **Severity: MEDIUM** — this is a standard vim motion that users will expect. The `G` mapping works for the last line, but the first-line `gg` is missing.

### Finding [M6] — Operator-pending `0` ignores pending count

`crates/narwhal-vim/src/machine.rs:216-225` — In operator-pending mode, pressing `0` immediately emits `Action::Operate { motion: LineStart, count: 1 }` without checking whether there's a pending count. In vim, `d0` deletes to start of line (correct), but `3d0` should still delete to start of line (count is meaningless for `0`). The current code does the right thing, but the pending count is silently dropped without being consumed. This matches vim behavior. **Severity: INFO** — correct behavior, just noting the subtle count interaction.

### Finding [M7] — Visual mode motions ignore count prefix

`crates/narwhal-vim/src/machine.rs:162-182` — All motions in visual mode are hardcoded with `count: 1`. In vim, `3j` in visual mode extends the selection by 3 lines. The current implementation only moves one line at a time. **Severity: LOW** — MVP acceptable, but power users will notice.

### Finding [L6] — No register/yank/delete implementation

The `Operator::Yank` and `Operator::Delete` produce `Action::Operate` but the `apply_action` method at `crates/narwhal-app/src/core/editor_dispatch/editor_keys.rs:139` has `Action::Operate { .. } => {}` — a no-op. Yank/delete operations are acknowledged but not wired. **Severity: LOW** — documented as v2.1 scope; `dd`/`yy`/`cc` are parsed but not applied.

---

## 13. Diagram Render

### Verdict: CLEAN — correct cardinality logic, FK composite handling

- **Cardinality**: `cardinality_for` at `crates/narwhal-diagram/src/build.rs:150-167` correctly derives cardinality from FK nullability and uniqueness.
- **Composite FK nullability**: `fk.columns.iter().any(|name| ... c.nullable)` — a composite FK is nullable iff *any* column is nullable. This matches the docstring: "the row can still link to a parent on the non-null part."
- **Composite FK uniqueness**: `fk_is_unique` at line 169 checks PK match, then unique indexes, then unique constraints, using `HashSet` comparison (order-insensitive). This correctly identifies composite unique constraints over FK columns.
- **Mermaid render**: Logical edges use `..` instead of `--` for dashed lines. Cardinality tokens are correct.

### Finding [I7] — Cross-schema FK references silently dropped

`crates/narwhal-diagram/src/build.rs:53-56` — FKs whose referenced table is not in the `tables` slice are dropped with no diagnostic. This is documented as "intentional so renderers never emit dangling edges" but the user gets no feedback about the dropped edge. The `BuildDiagnostic` enum exists for logical relations but not for dropped FKs. **Severity: INFO** — by design in v2.0; a follow-up could add a `DroppedFk` diagnostic.

---

## 14. Pivot

### Verdict: CORRECT — well-tested, clean NULL handling

- **Agg variants**: Count/Sum/Avg/Min/Max all implemented with `Accumulator` struct. `Count` works on any type; numeric aggregators check `has_numeric` before rendering.
- **NULL handling**: `value_as_f64` returns `None` for `Value::Null`. The accumulator's `ingest` increments `count` even for `None` values (important for `Count` and `Avg` denominator). Numeric aggregators skip null cells gracefully.
- **High-cardinality guard**: `max_cols` (default 50) collapses overflow into `(other)` bucket with merged accumulator.
- **`is_grid_unsafe`**: Shared between pivot and chart crates for control-character filtering.

### Finding [M8] — `derive_pivot_table` runs in O(rows × cols) per render

The full-row walk in `column_is_numeric` and the per-row aggregation both scan the entire result set on every render call. For streaming results with 100K+ rows, this recomputes the entire pivot on every frame. The docstring at `crates/narwhal-pivot/src/lib.rs:1-11` acknowledges this as a trade-off for correctness. **Severity: MEDIUM** — acceptable for v2.0 but will become a bottleneck for large streaming results. An incremental accumulator would solve this.

---

## 15. Chart

### Verdict: CORRECT — top-N by magnitude, tail truncation

- **Bar top-N**: `DEFAULT_BAR_BOUND = 50`. `derive_chart_data` at `crates/narwhal-app/src/core/chart.rs:147-150` sorts by `abs(value)` and truncates to `bound`, keeping the largest-magnitude entries regardless of sign.
- **Line/sparkline tail**: `DEFAULT_LINE_BOUND = 1_000`. Drains from the front, keeping the most recent `bound` entries.
- **Configurable**: `ChartConfig::bounded_to` is set from `ChartKind::default_bound()` but can be overridden.

### Finding [I8] — Bar chart sort is destructive (reorders original data)

`crates/narwhal-app/src/core/chart.rs:147` — `points.sort_by(|a, b| b.1.abs().partial_cmp(&a.1.abs()).unwrap_or(Ordering::Equal))` sorts the `points` vec in-place before truncating. Since `points` is built locally and consumed immediately, this is safe. However, the sort loses the original row-order, so bar charts always display by magnitude, not by the order the data arrived. This is the intended behavior. **Severity: INFO**.

### Finding [L7] — Magic numbers 50 and 1000 are not exposed to user config

The chart bounds are derived from `ChartKind::default_bound()` and stored in `ChartConfig::bounded_to`, but there's no `:chart bar --bound 20` command-line option. Users who want fewer/more bars are stuck with the defaults. **Severity: LOW** — a `--bound` flag would be a simple addition.

---

## 16. Drag-Flow / Pagination

### Verdict: CLEAN — well-structured

- **Yank**: `yank_cell` / `yank_row` copy values to the clipboard via `self.deps.clipboard.set()`.
- **Row CRUD**: Cell edit → commit → `PendingChanges` → `:submit` / `:revert`. The `pending` field on `Tab` tracks mutations.
- **Pagination**: `next_page` / `prev_page` use `RowSource.offset` and `page_size` to construct `LIMIT/OFFSET` queries. `set_page_size` updates per-tab.
- **JSON viewer**: Renders pretty-printed JSON with scroll. `parse_error` fallback shows raw text.

### Finding [L8] — `page_size` not validated

`crates/narwhal-app/src/core/editor_dispatch/sidebar.rs:139` — `set_page_size` accepts any `usize` including 0, which would produce `LIMIT 0` queries. A page size of 0 should be rejected or clamped to a minimum of 1. **Severity: LOW** — a `LIMIT 0` query is harmless (returns no rows) but is confusing UX.

---

## Summary Table

| ID  | Severity | File                                    | Line  | Issue |
|-----|----------|-----------------------------------------|-------|-------|
| M5  | MEDIUM   | `narwhal-vim/src/machine.rs`            | —     | `gg` motion not implemented |
| M8  | MEDIUM   | `narwhal-pivot/src/lib.rs`              | —     | O(rows × cols) pivot recomputation per render |
| L1  | LOW      | `narwhal-app/src/core/run_loop.rs`      | 91-96 | `swap_remove(0)` intermediate state fragile |
| L2  | LOW      | `narwhal-domain/src/editor.rs`          | —     | No spatial collision detection for secondaries after delete |
| L3  | LOW      | `narwhal-app/src/core/sessions.rs`      | —     | `ConnectionClosed` not emitted on crash |
| L4  | LOW      | `narwhal-app/src/persist/paths.rs`      | —     | Per-pid fallback files never cleaned up |
| L5  | LOW      | `narwhal-app/src/core/dispatch.rs`      | 223   | Mouse scroll moves cursor instead of viewport |
| L6  | LOW      | `narwhal-vim/src/action.rs` / `machine` | —     | Yank/delete operators produce actions but `apply_action` is no-op |
| L7  | LOW      | `narwhal-app/src/core/chart.rs`         | —     | Chart bounds not configurable from `:chart` command |
| L8  | LOW      | `narwhal-app/src/core/editor_dispatch/sidebar.rs` | 139 | `page_size` not validated (0 accepted) |
| I1  | INFO     | `narwhal-app/src/core/dispatch.rs`      | 45    | `await_pending_session_opens_sync` blocks key handler |
| I2  | INFO     | `narwhal-app/src/core/sessions.rs`      | 152   | `JoinHandle` from `schedule_schema_refresh` not awaited |
| I3  | INFO     | `narwhal-app/src/core/editor_dispatch/sidebar.rs` | 201   | `describe_table_into_result` blocks event loop |
| I4  | INFO     | `narwhal-app/src/core/state/tab.rs`     | 102   | Treesitter `reparse` not fed incremental edit deltas |
| I5  | INFO     | `narwhal-app/src/core/plugin_executor.rs` | —     | No timeout on `sql_run` execution |
| I6  | INFO     | `narwhal-app/src/core/dispatch.rs`      | 42-43 | Pre-read of `pending_count` avoids borrow conflict |
| I7  | INFO     | `narwhal-diagram/src/build.rs`          | 53    | Cross-schema FKs silently dropped with no diagnostic |
| I8  | INFO     | `narwhal-app/src/core/chart.rs`         | 147   | Bar chart sort is destructive (intended) |
| M2  | INFO     | `narwhal-app/src/core/sessions.rs`      | 88/103| Poison recovery uses `into_inner` (standard pattern) |
| M3  | INFO     | `narwhal-domain/src/editor.rs`          | 466   | Multi-cursor insert sorts after each char |
| M4  | INFO     | `narwhal-app/src/core/sessions.rs`      | 241   | Audit `Configuration` change wording inconsistent |
| M6  | INFO     | `narwhal-vim/src/machine.rs`            | 216   | Operator-pending `0` ignores pending count (correct) |
| M7  | INFO     | `narwhal-vim/src/machine.rs`            | 162   | Visual mode motions ignore count prefix |

---

## Architectural Observations

1. **Dispatch pattern is solid**: Single `&mut self` entry point, clean mutation → side-effect separation. The `meta_channel` / `run_channel` split prevents UI-blocking async work.

2. **State decomposition is well-factored**: `AppCore` is decomposed into `AppDeps` (immutable services), `ModalState` (overlay state), `SessionState` (data), `UiState` (appearance), `ProcessState` (lifecycle). Each sub-state has clear ownership and no cross-contamination.

3. **Multi-cursor MVP is clean**: The sorted `Vec<(usize, usize)>` with binary-search insertion, dedup, and primary-coincidence filtering is correct. The documented scope boundaries (no paste-into-multi-cursor, no vim-motion propagation, no undo-as-one-step) are honest.

4. **Vim machine needs `gg` and operator wiring**: The state machine is correct for what it implements, but `gg` (arguably the most common vim motion) and the yank/delete operator actions are stubs.

5. **Treesitter cache is the biggest performance opportunity**: Full reparse on every buffer-length change + no incremental edit deltas means syntax highlighting is O(buffer_size) per keystroke on changed buffers. For v2.0 this is fine; for v2.1 it should be optimized.

6. **Pivot per-frame recomputation is the second performance concern**: For streaming results with large row counts, deriving the entire pivot table on every render tick will become a bottleneck. An incremental accumulator would address this.
