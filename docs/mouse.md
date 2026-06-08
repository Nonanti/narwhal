# Mouse

Mouse handling is on by default. Configure with `[editor].mouse`:

| Value         | Behaviour                                          |
|---------------|----------------------------------------------------|
| `enabled`     | Full mouse support (default)                       |
| `click-only`  | Click positions the cursor; no drag or right-click |
| `disabled`    | All mouse events ignored                           |

## Editor pane

| Gesture          | Action                                             |
|------------------|----------------------------------------------------|
| Click            | Position the cursor                                |
| Drag             | Extend a character-wise selection                  |
| Double-click     | Select the word under the cursor                   |
| Triple-click     | Select the whole line                              |
| Middle-click     | Paste from the system clipboard                    |
| Right-click      | Open the editor context menu                       |
| Scroll wheel     | Scroll the editor                                  |

Selection is grapheme-aware. Multi-byte characters (Turkish, CJK,
accented Latin) land on display-column boundaries — clicks never
produce mid-codepoint cursors.

The right-click menu carries Cut / Copy / Paste / Select All /
Run Selection / Find / Toggle Comment.

## Results pane

| Gesture                | Action                                         |
|------------------------|------------------------------------------------|
| Click on a cell        | Move the focus to that row / column            |
| Click on a column hdr  | Sort by that column (toggles asc/desc/none)    |
| Right-click on a cell  | Open the cell context menu                     |
| Scroll wheel           | Scroll the results                             |

## Sidebar

| Gesture       | Action                                              |
|---------------|-----------------------------------------------------|
| Click         | Focus the row                                       |
| Double-click  | Expand a schema, or open a table in TableDetail     |
| Right-click   | Open the sidebar context menu                       |

## Tabs

| Gesture       | Action                                              |
|---------------|-----------------------------------------------------|
| Click         | Switch to the tab                                   |
| Middle-click  | Close the tab                                       |

## Notes

- All mouse paths respect `[editor].mouse`. Setting it to `disabled`
  blocks every mouse event, including scroll and pane focus changes.
- Mouse events are suppressed while a modal is open. The modal owns
  the keyboard and any background pane state is left untouched.
- macOS Terminal.app does not forward all mouse modes; iTerm2,
  Alacritty, WezTerm, and Kitty are fully supported.
