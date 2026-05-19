# Plan 07-03 — Multi-statement output tabs

## Why

Running `SELECT 1; SELECT 2;` today shows… one of the results,
and the other vanishes. Multi-statement scripts are common
(migrations, comparison queries, "delete the test rows then
verify they're gone") and the user needs to see every output,
not the last one to land.

## Scope

When the dispatch pipeline produces N result sets, the result
pane gains a small tab strip at the top:

```
[ result 1/3 ]  result 2/3  result 3/3   ← strip
─────────────────────────────────────────
... grid for the active result ...
```

- The active tab is highlighted with `theme.accent`.
- `]r` jumps to the next result; `[r` to the previous.
- `Ctrl-PgDown` / `Ctrl-PgUp` mouse-friendly aliases.
- Clicking on a tab in the strip (depends on 06-02 mouse) jumps
  to it.
- Status bar shows `result 2 of 5` while the strip is visible.

Each result tab carries its own `ResultView` state — scroll
position, selected row, search highlight, filter, sort (06-04),
row detail modal (07-02). Switching tabs preserves all of it.

When only one result lands (the common case), no strip renders.

## Constraints

- AGENTS.md: no `unwrap()` / `expect()` in production code.
- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- One conventional commit, long-form.
- Backwards compatible: callers that produce a single result set
  see no behavioural change.

## Concrete steps

### Step 1: ResultBundle

```rust
pub struct ResultBundle {
    pub results: Vec<ResultView>,
    pub active: usize,
}

impl ResultBundle {
    pub fn single(view: ResultView) -> Self { ... }
    pub fn active(&self) -> &ResultView { &self.results[self.active] }
    pub fn active_mut(&mut self) -> &mut ResultView { &mut self.results[self.active] }
    pub fn next(&mut self) { self.active = (self.active + 1) % self.results.len(); }
    pub fn prev(&mut self) {
        self.active = self.active.checked_sub(1)
            .unwrap_or(self.results.len() - 1);
    }
}
```

Replace `Tab::result_view: ResultView` with
`Tab::results: ResultBundle`. Where the existing code accessed
`tab.result_view`, redirect to `tab.results.active()` /
`tab.results.active_mut()`.

### Step 2: dispatch path

`run.rs::dispatch_batch` already iterates over the parsed
statements. Today it overwrites `tab.result_view` on each loop
iteration; change it to push into a `Vec<ResultView>` and build a
`ResultBundle` at the end with `active = 0`.

Streaming dispatch (`F7`) produces a single result by construction
— no change needed there.

### Step 3: key routing

`handle_results_key`:
- `]` followed by `r` → `tab.results.next()`
- `[` followed by `r` → `tab.results.prev()`

`handle_global_key`:
- `Ctrl-PgDown` → next; `Ctrl-PgUp` → prev (works regardless of
  focus)

### Step 4: render tab strip

`widgets/results.rs::render_results` gains a top row when
`bundle.results.len() > 1`. The strip is a `Tabs` widget from
ratatui showing `result {n}` per result; the active one gets
the accent style.

Mouse hit-test (`LayoutRegions::result_tabs: Vec<(Rect, usize)>`)
exposes the per-tab rects so 06-02's click path can route them.

### Step 5: tests

`tests/multi_statement.rs`:

1. `single_result_no_strip` — dispatch one SELECT, assert
   `bundle.results.len() == 1` and the strip-rect Vec is empty.
2. `three_statements_three_results` — dispatch `SELECT 1; SELECT
   2; SELECT 3;`, assert `bundle.results.len() == 3`.
3. `]r_advances_active` — three results, dispatch `]r`, assert
   `bundle.active == 1`. Dispatch `]r` twice more, assert `active
   == 0` (wrap).
4. `[r_wraps_backward` — three results, dispatch `[r`, assert
   `active == 2`.
5. `state_preserved_across_tab_switch` — three results, scroll
   to row 3 in tab 1, switch to tab 2, switch back, assert tab 1
   still on row 3.

Acceptance: +5 tests.

## Files

- `crates/narwhal-app/src/core.rs` (ResultBundle, replace
  `tab.result_view`, dispatch path, key routing)
- `crates/narwhal-app/src/run.rs` (dispatch_batch builds bundle)
- `crates/narwhal-tui/src/widgets/results.rs` (render the strip)
- `crates/narwhal-tui/src/layout.rs` (LayoutRegions::result_tabs)
- `crates/narwhal-app/tests/multi_statement.rs` (new)

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean
- `nix develop --command cargo test --all` reports +5 from baseline
- Manual smoke: type `SELECT 1; SELECT 2; SELECT 3;`, run with
  F6, observe a three-tab strip; `]r` cycles forward.

## Commit message template

```
feat(results): tab strip for multi-statement outputs

Running SELECT 1; SELECT 2; today shows one of the results and
the other vanishes — every statement after the first overwrote
the result_view in dispatch_batch. Migration scripts and
comparison queries hit this constantly.

ResultBundle replaces Tab::result_view: ResultView with
Tab::results: ResultBundle holding Vec<ResultView> + an active
index. dispatch_batch pushes one ResultView per parsed
statement; when more than one lands, the result pane renders a
tab strip at the top:

  [ result 1/3 ]  result 2/3  result 3/3

The active tab is theme.accent. ]r / [r cycle forward / back
(with wrap); Ctrl-PgDown / Ctrl-PgUp are the keyboard-shortcut
equivalents that work regardless of focus; clicking a tab when
mouse support is enabled (06-02) jumps directly.

Each tab carries its own ResultView state — scroll position,
selected row, search highlight, plan-04 filter / sort, plan-02
row detail modal. Switching tabs preserves every bit of it.

When only one result lands (the common case), no strip renders
and the layout is byte-for-byte identical to the prior single-
result path; the change is invisible.

Five new tests cover the single-result no-strip path, three-
statement bundle build, ]r / [r wrap, and state preservation
across tab switches.
```
