# Plan 06-05 — Query history with Ctrl+R modal

## Why

narwhal already writes every executed statement to
`~/.local/share/narwhal/history.jsonl` through the `Journal`
service. The data is there; the UI to browse it isn't. DataGrip
has a "History" panel and a Ctrl+R hotkey to jump back through
recent queries.

## Scope

- `Ctrl+R` (anywhere outside the wizard) or `:history` opens a
  centred modal listing the most recent N=200 entries from the
  journal, newest first.
- Each entry shows: timestamp, connection name, single-line
  statement preview (truncated to terminal width).
- Typing a substring filters the list (case-insensitive). The
  filter buffer appears at the top of the modal.
- Arrow keys / Ctrl-J / Ctrl-K cycle the highlight.
- Enter inserts the selected statement into the editor (insert
  mode) at the cursor.
- Shift+Enter inserts AND runs immediately.
- Esc dismisses without changing anything.

## Constraints

- The journal already records to disk asynchronously — the modal
  reads back via the existing `Journal::recent(n)` method (or
  adds one if absent).
- One commit, conventional, long-form.
- `clippy --all-targets -- -D warnings`, `fmt --check`, no
  `unwrap`/`expect` in production.

## Concrete steps

### Step 1: Journal::recent(n)

Inspect `crates/narwhal-history/src/lib.rs`. If a `recent(n)` API
isn't there, add it: parse the JSONL file tail and return up to
`n` entries newest-first. Use an `Iterator::rev` over the lines
rather than reading the whole file when feasible.

```rust
pub struct HistoryEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub connection: String,
    pub sql: String,
}

impl Journal {
    pub async fn recent(&self, n: usize) -> Vec<HistoryEntry> { ... }
}
```

### Step 2: HistoryState on AppCore

```rust
pub struct HistoryState {
    pub entries: Vec<HistoryEntry>,
    pub filter: String,
    pub selected: usize,
}

// ... on AppCore:
pub history: Option<HistoryState>,
```

`history.is_some()` means the modal is open.

### Step 3: open / close lifecycle

```rust
async fn open_history(&mut self) {
    let entries = match &self.history_journal {
        Some(j) => j.recent(200).await,
        None => {
            self.status.message = "history disabled".into();
            return;
        }
    };
    self.history = Some(HistoryState {
        entries,
        filter: String::new(),
        selected: 0,
    });
}
```

`Ctrl+R` keybind: route in `handle_global_key` to
`self.open_history()`. Note: opening requires async because of
`Journal::recent`; either make `handle_global_key` async or use
the same `block_in_place + Handle::current().block_on` pattern
the other sync→async bridges use.

### Step 4: input dispatch when modal open

`handle_key` checks `self.history.is_some()` before the existing
branches and routes to `handle_history_key`:

- char        → append to filter, recompute visible_entries
- Backspace   → pop filter char
- Up / Ctrl-K → selected -= 1 (wrap)
- Down / Ctrl-J → selected += 1 (wrap)
- Enter       → insert the entry's `sql` into the editor at the
                cursor, close modal
- Shift+Enter → insert + dispatch run
- Esc         → close modal

### Step 5: render

New widget in `narwhal-tui` (`history_modal`):

- Centred Rect (60% width × 70% height, max 80×24)
- Top row: ` history · <N>/<total>  filter: <buf>_ `
- Body: a Table with three columns
  - timestamp (Length 19, `2026-05-19 03:42:17`)
  - connection (Length 12)
  - sql preview (Min 20, truncated with ellipsis)
- Selected row reverse-video (theme.accent bg)

Filtered subset: entries whose `sql` (case-insensitive) contains
the filter string.

### Step 6: tests

`tests/history.rs`:

1. `history_opens_with_journal_entries`: seed the journal with
   3 entries, open the modal, assert state.entries.len() == 3.
2. `history_filter_narrows_visible`: type a substring that matches
   only 1 of the 3, assert visible subset size 1.
3. `history_enter_inserts_sql_into_editor`: select an entry,
   press Enter, assert editor.entire_text() contains the entry's
   sql and the modal closed.
4. `history_esc_closes_without_change`: open + Esc, assert the
   modal closed and editor unchanged.
5. `history_no_journal_shows_message`: AppCore without a journal,
   open history, assert status message and `history` stays None.

Acceptance: test count rises by **5**.

## Files

- `crates/narwhal-history/src/lib.rs` (Journal::recent if missing)
- `crates/narwhal-app/src/core.rs` (HistoryState, open/close,
  handle_history_key)
- `crates/narwhal-tui/src/widgets/history.rs` (new)
- `crates/narwhal-tui/src/lib.rs` (export the new widget)
- `crates/narwhal-tui/src/layout.rs` (render the modal overlay
  when state present)
- `crates/narwhal-app/tests/history.rs` (new)

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` reports +5 from baseline.
- Manual smoke: `Ctrl+R` opens a modal with my recent queries; I
  can type to filter, Enter to insert into the editor.

## Commit message template

```
feat(history): Ctrl+R modal for browsing the query journal

narwhal already writes every executed statement to
~/.local/share/narwhal/history.jsonl through the Journal service.
The data was there; the UI to browse it wasn't.

Ctrl+R (or :history) opens a centred modal with the 200 newest
journal entries. Typing filters the list by case-insensitive
substring across the SQL preview. Up/Down (or Ctrl-J/K) cycle the
highlight. Enter inserts the selected statement into the editor
at the cursor; Shift+Enter inserts and runs. Esc dismisses
without changing anything.

Journal::recent(n) added when missing — reads the JSONL tail in
reverse without slurping the whole file.

The modal lives in narwhal-tui::widgets::history; layout overlays
it on top of the existing TUI when AppCore::history is Some.

Five new tests cover open, filter, accept, dismiss, and the
no-journal status message branch.
```
