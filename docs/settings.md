# Settings

narwhal reads `~/.config/narwhal/config.toml` at start-up and watches
it for changes while running (live-reload via `notify`). The in-app
`:settings` modal exposes the most-used knobs without leaving the
TUI; advanced fields (audit, plugins, vault, LSP, …) stay
file-only.

## Layout

```toml
schema_version = 2

theme = "dark"               # dark | light | high-contrast

[editor]
mode = "vim"                 # vim | basic | emacs (see editor-modes.md)
mouse = "enabled"            # enabled | click-only | disabled
tab_width = 4
use_spaces = true
line_numbers = true
show_mode_indicator = true
auto_indent = true
highlight_current_line = false
scroll_off = 3
word_wrap = false

[keybindings]
preset = "default"           # default | vscode | datagrip | intellij
leader = "\\"

[diagram]
icons = "ascii"              # ascii | nerdfont

[run]
batch_size = 64
stream_flush_ms = 50
```

Section-by-section reference below. Every field has a documented
default — omit a knob and you get the default behaviour.

## `[editor]`

| Field                    | Default     | Description                          |
| ------------------------ | ----------- | ------------------------------------ |
| `mode`                   | `"vim"`     | Editor input model. See [editor-modes.md](./editor-modes.md). |
| `mouse`                  | `"enabled"` | Mouse behaviour inside the editor. See [mouse.md](./mouse.md). |
| `tab_width`              | `4`         | Cells per `Tab`.                     |
| `use_spaces`             | `true`      | `Tab` inserts spaces instead of `\t`. |
| `line_numbers`           | `true`      | Render the line-number gutter.       |
| `show_mode_indicator`    | `true`      | Render the `NORMAL` / `BASIC` / `EMACS` segment in the status bar. |
| `auto_indent`            | `true`      | New lines inherit the leading whitespace of the previous line. |
| `highlight_current_line` | `false`     | Paint the cursor row with a dimmed background. |
| `scroll_off`             | `3`         | Keep this many lines visible above/below the cursor before scrolling. |
| `word_wrap`              | `false`     | Wrap long lines visually (buffer is never reflowed). |

## `[keybindings]`

| Field    | Default     | Description                                   |
| -------- | ----------- | --------------------------------------------- |
| `preset` | `"default"` | Layered IDE-style chord set. See below.       |
| `leader` | `"\\"`     | Vim leader key. Empty string disables leader. |

### Presets

Each preset layers a small set of additional chords on top of the
built-in defaults. Your own `[keymap.*]` overrides always win.

| Preset      | Extra chords                                              |
| ----------- | --------------------------------------------------------- |
| `default`   | none                                                      |
| `vscode`    | `Ctrl+P` opens goto, `Ctrl+Shift+P` opens command palette |
| `datagrip`  | `Ctrl+B` focuses the sidebar, `Ctrl+Enter` runs the statement |
| `intellij`  | same as `datagrip`                                        |

### Per-group keymap overrides

You can rebind individual chords inside `[keymap.<group>]` tables:

```toml
[keymap.results]
"ctrl+s"    = "results-commit-pending"
"K"         = "results-prev-tab"
"shift+tab" = "results-prev-cell"

[keymap.editor]
"ctrl+space" = "editor-trigger-completion"
```

Action names are `kebab-case`. Setting an action to `"unbind"`
removes the binding. Unknown chords or actions surface at start-up as
a status-bar warning; the rest of the bindings still load. Available
groups: `global`, `editor`, `sidebar`, `results`, `row-detail`,
`cell-popup`, `json-viewer`, `pending-preview`.

## `:settings` modal

`:settings` (alias `:set`) opens the in-app editor. Sections cycle
with `Tab` / `Shift+Tab`; fields with `↑` / `↓` or `j` / `k`. Space
or Enter toggles the highlighted field; Ctrl+S persists to disk and
re-applies the draft; Esc discards.

The modal footer flips to a warning colour the moment the draft
diverges from disk, so you can tell at a glance whether unsaved
changes are pending.

## `:mode` quick-switch

`:mode vim|basic|emacs` flips the editor input model and saves the
new state to `config.toml` in one shot — no modal, no extra
keystrokes. Useful for muscle-memory toggles during the workday.

## Live reload

A `notify`-driven watcher polls `~/.config/narwhal/config.toml`
through the parent directory. Saves from any external editor flow
back into the running app within ~50 ms. Self-writes from the
in-app modal are suppressed inside a 750 ms window so the modal's
Ctrl+S doesn't echo through the watcher.

If the watcher fails to start (sandbox without inotify, missing
file, …), narwhal logs a warning and runs with live-reload
disabled. The settings modal still works.

## Migration from v1

Older configs use `keybindings.vim_mode = true|false` instead of
`editor.mode`. The deprecated bit is still read: a v1 file with
`vim_mode = false` is interpreted as `editor.mode = "basic"` at
runtime, no migration pass required. The CLI `narwhal migrate-config`
rewrites the file into the v2 shape when you want to clean up.
