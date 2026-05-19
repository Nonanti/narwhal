# Plan 07-02 — Row detail / form view

## Why

The cell popup (Enter) shows one cell's value. For wide tables
with 20+ columns, reading a single row across the result grid is
unworkable — the user has to scroll left/right repeatedly,
losing track of which row they were on. DataGrip's "Row Editor"
side-panel is the standard solution.

## Scope

A new full-screen-overlay modal that shows every column of the
focused row as a `column_name = value` list, scrollable.

- `R` (or Shift+Enter) on the focused row opens the modal.
- Each entry is rendered as:
  ```
  column_name (TYPE)
    value (potentially multi-line, wrapped to modal width)
  ```
- Up / Down / j / k navigate between columns.
- PgUp / PgDn scroll a page.
- `g` / `G` jump to top / bottom.
- The current column highlighted with `theme.accent` background.
- Esc / `R` / Shift+Enter dismisses.
- v1 is **read-only**. Editing rows from this view is v1.1 — the
  existing cell editor (`e` / `cw`) is the path for now.

Multi-line cell values get full `Paragraph` wrap (no glyph
projection) since the modal has room for them.

NULL cells render as `<null>` in `theme.muted`.

## Constraints

- AGENTS.md: no `unwrap()` / `expect()` in production code.
- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- One conventional commit, long-form.
- Must compose with existing modal stack (cell popup, completion,
  history, help) — row detail sits at the same layer as cell
  popup. Both shouldn't be open at once.

## Concrete steps

### Step 1: RowDetailState

```rust
pub struct RowDetailState {
    pub row_index: usize,
    pub columns: Vec<ColumnHeader>,
    pub values: Vec<Value>,
    pub selected_column: usize,
    pub scroll_offset: u16,
}
```

Lives on the active `Tab`:

```rust
// On Tab struct
pub row_detail: Option<RowDetailState>,
```

`row_detail.is_some()` means the modal is open. Opening with the
modal already open is a no-op (the existing modal stays).

### Step 2: open / close lifecycle

```rust
fn open_row_detail(&mut self) {
    let tab = &mut self.tabs[self.active_tab];
    let view = &tab.result_view;
    let Some(row_idx) = view.state.selected() else {
        self.status.message = "no row selected".into();
        return;
    };
    let (cols, rows) = match view.snapshot() {
        Some((c, r)) => (c, r),
        None => {
            self.status.message = "no result to inspect".into();
            return;
        }
    };
    let Some(row) = rows.get(row_idx) else {
        return;
    };
    tab.row_detail = Some(RowDetailState {
        row_index: row_idx,
        columns: cols.to_vec(),
        values: row.0.clone(),
        selected_column: 0,
        scroll_offset: 0,
    });
}
```

### Step 3: key routing

In `handle_results_key` (when modal not open):
- `KeyCode::Char('R')` → `open_row_detail()`
- `KeyCode::Enter` + Shift → `open_row_detail()`

When `row_detail.is_some()`, route keys to `handle_row_detail_key`
*before* the existing branches:
- Up / `k` → selected -= 1 (clamp)
- Down / `j` → selected += 1 (clamp)
- PgUp → selected -= page
- PgDn → selected += page
- `g` → selected = 0
- `G` → selected = last
- Esc / `R` / Shift+Enter → close

### Step 4: render

New widget in `narwhal-tui::widgets::row_detail`:

```rust
pub struct RowDetailView<'a> {
    pub columns: &'a [ColumnHeader],
    pub values: &'a [Value],
    pub selected_column: usize,
    pub scroll_offset: u16,
}

pub fn render_row_detail(frame: &mut Frame, area: Rect, view: RowDetailView, theme: &Theme)
```

Layout:
- Centred Rect, max 80 × 30 or 70% of screen (whichever smaller)
- `Clear` widget under the modal to mask the result pane
- Block border with title `row N · esc closes`
- Body: `List` of `(name, type, value)` entries, each a
  multi-line item

### Step 5: tests

`tests/row_detail.rs`:

1. `open_with_no_row_shows_status_message` — no selection,
   press `R`, assert status message + modal not open.
2. `open_populates_columns_and_values` — seed result, press `R`,
   assert state.values.len() == result column count.
3. `navigate_selects_columns` — open, press j twice, assert
   selected_column == 2.
4. `esc_closes` — open, press Esc, assert row_detail is None.

Acceptance: +4 tests.

## Files

- `crates/narwhal-tui/src/widgets/row_detail.rs` (new)
- `crates/narwhal-tui/src/widgets.rs` (re-export)
- `crates/narwhal-tui/src/lib.rs` (re-export)
- `crates/narwhal-tui/src/layout.rs` (overlay when state present)
- `crates/narwhal-app/src/core.rs` (RowDetailState, open/close,
  key routing, focus override during modal)
- `crates/narwhal-app/tests/row_detail.rs` (new)

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean
- `nix develop --command cargo test --all` reports +4 from baseline
- Manual smoke: select a row with 10+ columns, press `R`, scroll
  through every column on a single screen.

## Commit message template

```
feat(tui): row detail modal for wide result tables

The cell popup (Enter) shows one cell's value at a time; for
wide tables with 20+ columns, scrolling horizontally to read a
single row across the grid is unworkable. DataGrip's row editor
side-panel is the standard solution and now narwhal has its
equivalent.

R (or Shift+Enter) on the focused row opens a centred modal
that lists every column as

  column_name (TYPE)
    value (potentially multi-line, wrapped to modal width)

Up / Down / j / k navigate; PgUp / PgDn page; g / G jump to top /
bottom. The current entry highlights with theme.accent. Esc, R,
or Shift+Enter dismiss. NULL cells render as <null> in
theme.muted. Multi-line cell values get full Paragraph wrap
since the modal has the room — no glyph projection here.

v1 is read-only by design: the existing cell editor (e / cw) is
the path to mutate a value, and that flow already works against
the underlying row. Editing from the form is a v1.1 follow-up.

Four new tests cover the open-with-no-selection error path, the
state population, column navigation, and dismiss.
```
