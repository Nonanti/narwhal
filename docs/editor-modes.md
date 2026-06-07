# Editor Modes

narwhal ships three editor input models. Pick whichever fits the way
you already type:

| Mode    | Default in v2 | Style                   |
| ------- | :-----------: | ----------------------- |
| `vim`   |       ✓       | Normal / Insert / Visual / Command, vim chord vocabulary |
| `basic` |               | Modeless IDE-style: arrow keys, Ctrl+C/V/Z, Shift+arrow selects |
| `emacs` |               | Classic C-/M- chord set with `C-x` prefix              |

Switch at runtime:

```text
:mode vim
:mode basic
:mode emacs
```

The setting persists to `~/.config/narwhal/config.toml` and is
re-applied on the next start. The full settings modal (`:settings`)
exposes the same knob in the **Editor** section.

## Vim mode

Default. Same machinery the v1 editor used. See `F1` → *Editor (vim)*
for the live chord list. Highlights:

| Chord          | Action                              |
| -------------- | ----------------------------------- |
| `i` / `a`      | enter Insert mode                   |
| `Esc`          | back to Normal                      |
| `:`            | command palette                     |
| `dd` / `yy`    | delete / yank line                  |
| `v` / `V`      | character / line visual selection   |
| `Alt-N` / `Alt-A` | multi-cursor at next / all matches |

## Basic mode

Modeless. Plain typing inserts; arrow keys move; selection grows with
`Shift`.

| Chord                       | Action                            |
| --------------------------- | --------------------------------- |
| `←` / `→` / `↑` / `↓`        | move cursor                       |
| `Ctrl+←` / `Ctrl+→`         | word jump                         |
| `Shift+(arrow)`             | extend selection                  |
| `Home` / `End`              | beginning / end of line           |
| `Ctrl+Home` / `Ctrl+End`    | beginning / end of buffer         |
| `Ctrl+A`                    | select all                        |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | copy / cut / paste             |
| `Ctrl+Z` / `Ctrl+Y`         | undo / redo                       |
| `Ctrl+Shift+Z`              | redo (alt binding)                |
| `Ctrl+F` or `/`             | open editor search                |
| `Tab`                       | completion when inside a word, four-space indent otherwise |
| `Enter`                     | newline (auto-indent if enabled)  |
| `:`                         | command palette                   |
| `Esc`                       | clear selection / close popups    |

Typing over a selection replaces it, matching every GUI editor on the
planet.

## Emacs mode

Classic Ctrl- / Meta- chord set with a `C-x` two-stroke prefix. See
`F1` → *Editor (emacs)*.

| Chord            | Action                              |
| ---------------- | ----------------------------------- |
| `C-f` / `C-b`    | forward / backward char             |
| `C-n` / `C-p`    | next / previous line                |
| `C-a` / `C-e`    | beginning / end of line             |
| `M-f` / `M-b`    | forward / backward word             |
| `M-<` / `M->`    | beginning / end of buffer           |
| `C-Space`        | set mark (start a region)           |
| `C-w` / `M-w`    | kill / copy region                  |
| `C-y`            | yank (paste from clipboard)         |
| `C-k`            | kill to end of line                 |
| `C-d` / `M-d`    | delete char / word forward          |
| `C-/` or `C-_`   | undo                                |
| `C-s` / `C-r`    | forward / backward search           |
| `C-g`            | cancel (clear region / popup)       |
| `C-x C-s`        | submit / run current statement      |
| `C-x u`          | undo (alt binding)                  |
| `:`              | command palette                     |
| `Esc`            | clear region                        |

The `C-x` prefix sets a single-keystroke pending state; the status
indicator flips to `C-x` while it's armed.

## Status indicator

The status bar's left segment shows the current mode at a glance:

- Vim: `NORMAL`, `INSERT`, `VISUAL`, `CMD`, …
- Basic: `BASIC`
- Emacs: `EMACS`, or `C-x` while the prefix is armed

Disable the segment with `[editor].show_mode_indicator = false` (or
the matching toggle in `:settings → Editor`).

## Why three modes?

- **Vim** keeps the v1 muscle memory intact.
- **Basic** matches the feel of DataGrip, VS Code, IntelliJ — what
  almost everyone touches their first SQL editor in.
- **Emacs** because emacs users are persistent.

All three share the same buffer, the same selection model, the same
undo/redo history, and the same completion popup. Switching modes
mid-edit is supported and the buffer state survives unchanged.
