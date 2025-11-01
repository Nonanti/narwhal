# Plan 06-02 — Mouse support across all panes

## Why

narwhal is keyboard-only today. Modern terminal emulators and
multiplexers (kitty, alacritty, foot, wezterm, tmux 3.x, modern
xterm) all support mouse capture and report click/scroll events
through the same crossterm `Event::Mouse` channel narwhal already
reads. Refusing mouse is friction the user shouldn't have to
absorb when DataGrip and every web SQL client supports it
out-of-the-box.

## Scope

- Crossterm: enable mouse capture in raw mode (one flag on enter,
  one on leave).
- **Click on a pane** → focus changes to that pane (cheaper
  alternative to `Ctrl-W`).
- **Click on a sidebar table** → injects
  `SELECT * FROM <schema>.<table> LIMIT 100;` into the editor and
  dispatches `RunMode::Execute`.
- **Click on a completion popup item** → accepts that item (same
  effect as Tab/Enter).
- **Scroll wheel on the result grid** → vertical scroll.
- **Scroll wheel on the editor** → cursor-aware scroll (cursor
  stays on the same column, line offset moves).
- **Click on a result row** → select that row.
- **Click on a result cell** → select that cell (jumps both row
  and column).
- **Click on a result column header** → emit a sort cycle action
  (no-op until 06-04 lands; the dispatch site is still here so
  06-04 doesn't have to come back and re-touch the routing).
- Click anywhere else → no-op (no crashing on, e.g., border
  characters).

## Constraints

- Behaviour-preserving for the existing keyboard path. Every
  mouse-routed action must funnel through the same handlers the
  keyboard already uses so the two stay consistent.
- One commit, conventional, long-form.
- `clippy --all-targets -- -D warnings` clean, `fmt --check`.
- AGENTS.md: no `unwrap`/`expect` in production code.

## Concrete steps

### Step 1: enable mouse capture

In `crates/narwhal-app/src/terminal.rs` (the `TerminalGuard`
enter/leave path):

```rust
use crossterm::event::{EnableMouseCapture, DisableMouseCapture};
crossterm::execute!(io::stdout(), EnableMouseCapture)?;
// ... on Drop:
crossterm::execute!(io::stdout(), DisableMouseCapture)?;
```

### Step 2: track hit regions during layout

Every pane (sidebar, editor, results, status bar) draws into a
known `Rect`. The render path already computes them; expose them
through a `LayoutRegions` struct stored on `AppCore` (or returned
from `render()` and stashed on the next frame).

```rust
#[derive(Debug, Default, Clone)]
pub struct LayoutRegions {
    pub sidebar: Rect,
    pub editor: Rect,
    pub results: Rect,
    pub status: Rect,
    pub completion: Option<Rect>,
    pub sidebar_tables: Vec<(Rect, SidebarRow)>,
    pub result_header: Vec<(Rect, usize /* column index */)>,
    pub result_rows: Vec<(Rect, usize /* row index */)>,
    pub completion_items: Vec<(Rect, usize /* item index */)>,
}
```

The render functions in `narwhal-tui` build this struct as they
draw and return it. `AppCore::render` stashes it on
`self.last_layout`.

### Step 3: route MouseEvent

`AppCore::handle_event` gains a branch:

```rust
Event::Mouse(m) => self.handle_mouse(m),
```

`handle_mouse` dispatches on `MouseEventKind`:

- `Down(MouseButton::Left)` at (x, y) →
  - If inside `completion_items[i]`: replicate `Tab/Enter` accept.
  - Else if inside `sidebar_tables[i]`: inject + run the preview.
  - Else if inside `result_header[i]`: emit sort action (no-op
    until 06-04).
  - Else if inside `result_rows[i]`: select that row in the
    result view.
  - Else if inside one of the three pane Rects: change focus.
- `ScrollUp` / `ScrollDown`: dispatch to the pane under the
  cursor.
- `Up(_)`: no-op for now.
- `Moved`, `Drag(_)`: no-op (selection across rows is a stretch
  goal).

### Step 4: sidebar table click → preview

New helper `inject_table_preview(schema: &str, table: &str)` in
`core.rs`:

```rust
let sql = format!("SELECT * FROM {} LIMIT 100;\n",
    format_qualified_name(schema, table));
self.tabs[self.active_tab].editor.replace_all(&sql);
self.dispatch_current_statement(RunMode::Execute);
```

`format_qualified_name` quotes per the active driver's
identifier rules (already encapsulated in driver capabilities;
fall back to double-quote default if absent).

### Step 5: completion item click → accept

When the click is inside `completion_items[i]`, set
`state.selected = i` then call the existing accept path
(`handle_completion_key` with `Enter`).

## Files

- `crates/narwhal-app/src/terminal.rs` (mouse capture toggle)
- `crates/narwhal-app/src/app.rs` (event-loop dispatch)
- `crates/narwhal-app/src/core.rs` (handle_mouse, LayoutRegions
  field, inject_table_preview)
- `crates/narwhal-tui/src/layout.rs` (return LayoutRegions)
- `crates/narwhal-tui/src/widgets/results.rs`
- `crates/narwhal-tui/src/widgets/editor.rs`
- `crates/narwhal-tui/src/lib.rs` (sidebar render returns table
  rects)
- `crates/narwhal-app/tests/mouse.rs` (new)

## Tests

New `tests/mouse.rs`:

1. `pane_click_changes_focus`: click inside editor rect → focus is
   Editor; click inside sidebar rect → focus is Sidebar.
2. `sidebar_table_click_injects_preview`: seed a known schema,
   click a known table rect, assert editor contains
   `SELECT * FROM ... LIMIT 100;` and a query ran.
3. `completion_item_click_accepts`: open the popup with two
   items, click the second item rect, assert the buffer holds
   that item's text.
4. `scroll_in_results_pane_moves_view`: render a result with
   enough rows to scroll, send a ScrollDown event, assert the
   view's top offset increased.

Acceptance: total test count rises by **4**.

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` reports +4 from baseline.
- Manual smoke after build: mouse clicks visibly change focus,
  scroll wheel scrolls the result grid.

## Commit message template

```
feat(app,tui): mouse support across panes, sidebar, results, completion

Modern terminals all forward mouse events through crossterm's
Event::Mouse channel — refusing them is friction the user
shouldn't absorb. Enable mouse capture in raw mode and route the
events through the same handlers the keyboard path uses so the
two stay consistent.

- Click on a pane                → focus changes
- Click on a sidebar table       → injects SELECT * FROM <table>
                                   LIMIT 100; into the editor and
                                   runs it
- Click on a completion item     → accepts that item
- Click on a result row / cell   → moves the selection there
- Click on a result column header→ emits a sort cycle (no-op
                                   until plan 06-04 lands)
- Scroll wheel on result/editor  → vertical scroll

LayoutRegions struct tracks the screen Rect of every clickable
element during render; AppCore stashes it for the next event
loop iteration so MouseEvent.dispatch can hit-test in O(regions).

Four new tests cover the routing without a real terminal.
```
