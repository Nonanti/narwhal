# Plan 06-08 — Help panel (`?` / F1)

## Why

narwhal's keymap is now wide enough that nobody can remember it.
DataGrip has "Help → Keyboard Shortcuts PDF"; we can do better
with a live cheatsheet modal triggered by the universal `?` or
`F1`.

## Scope

- `?` in normal mode (any pane) or `F1` (anywhere, any mode)
  opens a centred modal listing every keybinding grouped by
  scope.
- Three sections: **Global**, **Editor**, **Results**.
- Each section is a two-column table: shortcut → description.
- Esc, `?`, F1, or a mouse click outside dismisses the modal.
- The content is a static `Cheatsheet` struct compiled into the
  binary; no introspection from the keymap struct in v1.

## Constraints

- Must not interfere with the existing modal stack (completion
  popup, history modal, wizard). When opened on top, the help
  modal is the topmost layer and intercepts Esc until dismissed.
- One commit, conventional, long-form.
- `clippy --all-targets -- -D warnings`, `fmt --check`, no
  `unwrap`/`expect` in production.

## Concrete steps

### Step 1: Cheatsheet content

`crates/narwhal-tui/src/widgets/help.rs` (new file):

```rust
pub struct CheatsheetEntry {
    pub keys: &'static str,
    pub description: &'static str,
}

pub struct CheatsheetSection {
    pub title: &'static str,
    pub entries: &'static [CheatsheetEntry],
}

pub const CHEATSHEET: &[CheatsheetSection] = &[
    CheatsheetSection {
        title: "Global",
        entries: &[
            CheatsheetEntry { keys: "F5 / Alt-Enter / Ctrl-;",
                              description: "run statement under cursor" },
            CheatsheetEntry { keys: "F6", description: "run whole buffer" },
            CheatsheetEntry { keys: "F7", description: "stream cursor statement" },
            CheatsheetEntry { keys: "F4 / Ctrl-C", description: "cancel running query" },
            CheatsheetEntry { keys: "Ctrl-W", description: "cycle pane focus" },
            CheatsheetEntry { keys: "Ctrl-T", description: "new editor tab" },
            CheatsheetEntry { keys: "Ctrl-Tab / Ctrl-Shift-Tab",
                              description: "cycle tabs" },
            CheatsheetEntry { keys: "Ctrl-R", description: "history modal (plan 06-05)" },
            CheatsheetEntry { keys: "? / F1", description: "this help" },
            CheatsheetEntry { keys: ":q", description: "quit" },
        ],
    },
    CheatsheetSection {
        title: "Editor",
        entries: &[
            CheatsheetEntry { keys: "i / a / o / O", description: "enter insert mode" },
            CheatsheetEntry { keys: "Esc", description: "back to normal mode" },
            CheatsheetEntry { keys: "Tab / Ctrl-Space", description: "completion" },
            CheatsheetEntry { keys: "↑ ↓ / Shift-Tab", description: "cycle popup items" },
            CheatsheetEntry { keys: "Enter / Tab (in popup)", description: "accept completion" },
            CheatsheetEntry { keys: "/ / ?", description: "forward / backward search" },
            CheatsheetEntry { keys: "n / N", description: "next / prev search match" },
            CheatsheetEntry { keys: ":s/old/new/g", description: "substitute on line" },
            CheatsheetEntry { keys: ":%s/old/new/g", description: "substitute in buffer" },
            CheatsheetEntry { keys: "yy / dd / p", description: "yank / delete / paste line" },
        ],
    },
    CheatsheetSection {
        title: "Results",
        entries: &[
            CheatsheetEntry { keys: "h j k l / arrows", description: "move selection" },
            CheatsheetEntry { keys: "Enter", description: "open cell popup" },
            CheatsheetEntry { keys: "e", description: "edit cell value" },
            CheatsheetEntry { keys: "y", description: "yank cell to clipboard" },
            CheatsheetEntry { keys: "s", description: "sort current column" },
            CheatsheetEntry { keys: "/", description: "filter rows" },
            CheatsheetEntry { keys: ":next / :prev", description: "page through results" },
        ],
    },
];
```

(Wording / shortcuts may diverge — verify against the actual
keymap before committing.)

### Step 2: HelpState

```rust
// On AppCore:
pub help_open: bool,
```

Toggle:

```rust
fn toggle_help(&mut self) {
    self.help_open = !self.help_open;
}
```

### Step 3: key routing

`handle_global_key` gains:

```rust
match key.code {
    CtKey::F(1) => { self.toggle_help(); return true; }
    CtKey::Char('?') if self.vim.mode() == Mode::Normal && self.focus != Pane::Editor => {
        self.toggle_help();
        return true;
    }
    _ => {}
}
```

`?` triggers from normal mode in any pane *except* the editor —
in the editor `?` is reverse-search (plan 06-06). F1 always
works regardless of pane / mode.

When `help_open == true`, the modal intercepts:

- Esc / `?` / F1 → close.
- Anything else → consumed but no-op (status message hint).

### Step 4: render

`render_help_modal(frame, screen)`:

- Centred Rect (max 60×24, otherwise 70% of available)
- Clear the underlying widgets.
- Block border with title ` help · esc closes `.
- Three sections rendered as labelled Tables.

### Step 5: snapshot test

`tests/snapshots.rs`: new test `snapshot_help_modal` that opens an
empty AppCore, sets `help_open = true`, renders, and asserts the
snapshot matches.

Acceptance: test count rises by **1**.

## Files

- `crates/narwhal-tui/src/widgets/help.rs` (new — CHEATSHEET +
  render_help_modal)
- `crates/narwhal-tui/src/widgets.rs` (re-export)
- `crates/narwhal-tui/src/lib.rs` (re-export)
- `crates/narwhal-tui/src/layout.rs` (overlay when help_open)
- `crates/narwhal-app/src/core.rs` (help_open field, toggle,
  routing)
- `crates/narwhal-app/tests/snapshots.rs` (snapshot)
- `crates/narwhal-app/tests/snapshots/snapshots__help_modal.snap`
  (new)

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` reports +1 from
  baseline.
- Manual smoke: F1 in any pane / mode → modal opens; Esc closes.

## Commit message template

```
feat(tui): help panel modal triggered by ? / F1

The keymap is now wide enough that nobody can remember it. ? in
normal mode (any pane except editor, where ? is reverse-search)
or F1 (anywhere, any mode) opens a centred modal listing every
keybinding grouped by scope: Global, Editor, Results.

Each section is a two-column table (keys → description) built
from a static CHEATSHEET const. No introspection from the keymap
struct in v1 — the content lives next to the binding code and is
updated by hand when the bindings change.

Esc / ? / F1 dismiss. The modal is the topmost layer in the
existing modal stack: completion popup, history modal, wizard
all sit below.

One new snapshot test pins the rendered modal so accidental
keymap docs drift fails CI.
```
