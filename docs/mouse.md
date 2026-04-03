# Mouse Support

Beyond the v1 click-to-focus behaviour, the editor pane now responds
to the full GUI-editor mouse vocabulary. Configurable via
`[editor].mouse` in `~/.config/narwhal/config.toml` or the `:settings`
modal.

| Mode          | Behaviour                                              |
| ------------- | ------------------------------------------------------ |
| `enabled`     | Default. Cursor positioning, drag selection, double / triple click, right-click menu, middle-click paste. |
| `click-only`  | Single-click positions the cursor; drag and multi-click are no-ops. |
| `disabled`    | The editor pane ignores all mouse events. v1 behaviour. |

## Chord vocabulary (when `enabled`)

| Action                          | Result                              |
| ------------------------------- | ----------------------------------- |
| Left-click inside the editor    | Position the cursor at the click cell |
| Left-click + drag               | Extend the selection from the click anchor |
| Double-click                    | Select the word under the cursor    |
| Triple-click                    | Select the entire line              |
| Middle-click                    | Paste the clipboard at the click position (X11 primary-selection style) |
| Right-click                     | Open the editor context menu        |
| Mouse wheel up / down           | Scroll editor or result pane (same as v1) |

## Context menu

Right-click opens an inline overlay with the canonical actions:

- **Cut** / **Copy** / **Paste** — same plumbing as the keyboard
  bindings; disabled entries grey out when there's no selection or
  the clipboard is empty.
- **Select All**
- **Run Selection** — runs the current statement; if a selection is
  active, only the highlighted region is sent to the driver.
- **Find** — opens the editor search prompt.
- **Toggle Comment** — prepends or strips a `--` comment on every
  line touched by the selection (or the current line when nothing is
  selected). The toggle decides add vs strip based on whether every
  affected line is already commented.

Navigate the menu with `↑` / `↓` / `j` / `k`; accept with `Enter` or
`Space`; close with `Esc`. Disabled entries are skipped during
navigation.

## Terminal emulator notes

- **kitty**, **WezTerm**, **Alacritty** and **iTerm2** report all
  the events narwhal needs out of the box.
- **GNOME Terminal** drops `Drag(Right)` and `Move` events on some
  releases; cursor positioning still works.
- **tmux** users: set `set -g mouse on` to forward events through to
  narwhal. Without this, only the host-side mouse capture sees them.

## Gutter offset

Clicks inside the line-number gutter snap the cursor to the start of
the clicked line, not column 0 of the buffer. The renderer reserves
a 6+ cell gutter; the click translator subtracts the gutter width
before mapping to a buffer column.

## Mode interaction

- **Vim mode**: mouse click positions the cursor without leaving the
  current vim mode. The selection from a drag becomes a vim Visual
  selection, ready for `d` / `y` / `:` / etc.
- **Basic mode**: a drag selection participates in `Ctrl+C` / `Ctrl+X`
  exactly like a Shift-arrow selection.
- **Emacs mode**: a drag selection is equivalent to setting the mark
  at the click anchor and moving the cursor to the drag head, so
  `C-w` / `M-w` / `C-y` work on it.
