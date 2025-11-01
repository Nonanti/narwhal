# Plan 06-04 — Result sort + filter

## Why

Staring at a 100-row result and not being able to sort it without
typing `ORDER BY` is a daily friction. Likewise for filtering down
to "rows where status = pending" without re-running the query.

Both are post-query operations on the materialised result set —
narwhal is not re-issuing SQL. They make exploration cheap.

## Scope

### Sort

- Column-header click (mouse) or `s` on the focused column
  (keyboard) cycles `None → Asc → Desc → None`.
- Sort is stable: rows with equal values in the active column keep
  their relative order from the previous tier.
- Visible indicator: header shows `▲` / `▼` next to the column
  name when sort is active.
- Sort works against the **value as stored** (`Value::Int(1)` is
  ordered numerically, not as the string `"1"`).

### Filter

- `/` in the result pane opens a small filter prompt at the
  bottom of the result widget.
- Typing filters the visible rows by case-insensitive substring
  across all columns (any column matches → row stays).
- Esc closes the filter and restores the full row set.
- Enter accepts the current filter and closes the prompt; the
  filter stays active until cleared with another `/` + Esc.

### Composition

- Filter applies first, sort applies to the filtered subset.
- Streamed results: sort/filter only work after the stream
  finishes (status bar says "sort/filter unavailable while
  streaming" if invoked mid-stream).

## Constraints

- Behaviour-preserving when sort/filter are inactive.
- Sort is in-memory; no SQL is re-issued.
- One commit, conventional, long-form.
- `clippy --all-targets -- -D warnings`, `fmt --check`, no
  `unwrap`/`expect` in production.

## Concrete steps

### Step 1: result view state

`ResultView` (or whatever owns the displayed rows) gains:

```rust
pub struct ResultViewState {
    pub sort: Option<(usize /* column */, SortDir)>,
    pub filter: String,
    pub filter_prompt_open: bool,
    // ... existing fields
}

pub enum SortDir { Asc, Desc }
```

### Step 2: apply transformations on render

Before rendering, derive the displayed row indices:

```rust
fn visible_rows(&self) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..self.rows.len()).collect();

    if !self.filter.is_empty() {
        let needle = self.filter.to_lowercase();
        indices.retain(|&i| {
            self.rows[i].0.iter().any(|v| {
                v.render().to_lowercase().contains(&needle)
            })
        });
    }

    if let Some((col, dir)) = self.sort {
        indices.sort_by(|&a, &b| {
            let av = self.rows[a].0.get(col);
            let bv = self.rows[b].0.get(col);
            let ord = compare_values(av, bv);
            match dir {
                SortDir::Asc => ord,
                SortDir::Desc => ord.reverse(),
            }
        });
    }

    indices
}
```

`compare_values`: cmp by `Value` variant — Int < Int numerically,
Float < Float, String < String lexicographically, Null sorts last.

### Step 3: keyboard bindings

- Result pane focused + `s` → toggle sort on the current column.
- Result pane focused + `/` → open filter prompt.
- Filter prompt open + char → append to filter, re-derive
  visible_rows.
- Filter prompt open + Backspace → pop char.
- Filter prompt open + Enter → close prompt, keep filter.
- Filter prompt open + Esc → close prompt, clear filter.

### Step 4: mouse bindings (depends on 06-02)

- Click on `result_header[col]` → toggle sort on that column.
  The hit-region for the column header is already exposed by 06-02.

### Step 5: render the sort indicator

In the header cell:

```rust
let label = match state.sort {
    Some((c, SortDir::Asc))  if c == i => format!("{} \u{25b2}", name),
    Some((c, SortDir::Desc)) if c == i => format!("{} \u{25bc}", name),
    _ => name.to_owned(),
};
```

### Step 6: render the filter prompt

Below the result grid, when `filter_prompt_open` is true, render
a one-row Paragraph: ` / <filter buffer>_`. When closed but
filter non-empty, show ` [filter: <buffer>]` in the result title
line so the user knows a filter is active.

### Step 7: streaming guard

`spawn_cancel`, `dispatch_*_stream`, `handle_run_update` flag the
result as "streaming" or "complete". `s` / `/` while streaming
sets a status message and no-ops.

## Files

- `crates/narwhal-app/src/core.rs` (result pane key dispatch,
  filter prompt state)
- `crates/narwhal-tui/src/widgets/results.rs` (sort indicator in
  header, filter row, visible-rows derivation)
- `crates/narwhal-app/tests/result_sort_filter.rs` (new)

## Tests

New `tests/result_sort_filter.rs`:

1. `sort_asc_then_desc_then_off`: seed a 5-row result, dispatch
   `s` three times, assert visible row order each time.
2. `sort_stable_across_ties`: two rows with the same value in the
   sort column, assert their relative order is preserved.
3. `sort_handles_nulls`: NULLs sort last in Asc and first in
   Desc.
4. `filter_substring_case_insensitive`: type "PEN" → rows whose
   any cell contains "pen" (case-insensitive) remain.
5. `filter_then_sort`: filter first, sort on remaining subset,
   assert order.
6. `escape_clears_filter_and_closes_prompt`: open prompt, type,
   Esc, assert filter empty and prompt closed.
7. `streaming_results_reject_sort`: start a stream, send `s`,
   assert status message "sort/filter unavailable while
   streaming".

Acceptance: test count rises by **7**.

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` reports +7 from baseline.
- Manual smoke after build: `s` cycles sort, `/` filters, both
  visible in the title / header.

## Commit message template

```
feat(results): column sort and substring filter

Staring at a 100-row result and having to type ORDER BY just to
see it sorted is a friction the user shouldn't have to absorb;
neither is re-running a WHERE clause when all you want is to
glance at a subset. Both are now in-memory operations on the
materialised result set.

- `s` on the focused result column (or click the column header
  once mouse support lands) cycles None → Asc → Desc → None. The
  cmp is variant-aware: Int sorts numerically, NULL sorts last,
  Strings sort lexicographically case-insensitive. Sort is stable
  across ties.
- `/` in the result pane opens a filter prompt; typing filters
  visible rows by case-insensitive substring across all columns.
  Esc closes + clears, Enter closes + keeps.
- Filter applies first; sort applies to the filtered subset.

Sort indicator (▲ / ▼) appears next to the active column header.
A persistent `[filter: <buf>]` tag appears in the result title
when a filter is active but the prompt is closed.

Streaming results reject both operations with a status message
until the stream completes; the materialised path is the v1
target.

Seven new tests cover each branch.
```
