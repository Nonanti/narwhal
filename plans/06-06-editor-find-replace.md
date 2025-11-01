# Plan 06-06 — Editor find / replace

## Why

Vim ships `/`, `?`, `n`, `N`, `:s/foo/bar/`, `:s/foo/bar/g`, and
`:%s/foo/bar/g` — and narwhal's vim layer implements none of them.
Users who type a complex query and then realise they need to
rename every reference to `users` to `accounts` reach for muscle
memory that fails.

## Scope

### Forward / backward search

- `/`  in normal mode → prompt at the bottom of the editor pane.
- `?`  in normal mode → same prompt, search direction reversed.
- Typing characters builds the search needle.
- Enter accepts the needle and jumps to the first match in the
  given direction. Cursor lands on the first byte of the match.
- Esc cancels the search; cursor returns to where it was when
  `/` / `?` was pressed.
- Once a search is active (needle non-empty), `n` repeats the
  search in the original direction, `N` in the reverse.
- Matches are highlighted (theme.accent background) while the
  search is "active". A second `/` or `:nohlsearch` clears the
  highlight.

### Substitute

- `:s/old/new/`     → replace the first occurrence on the current
                      line.
- `:s/old/new/g`    → replace every occurrence on the current line.
- `:%s/old/new/g`   → replace every occurrence in the whole
                      buffer.
- `:%s/old/new/gc`  → confirm each replacement (y/n/a/q at each
                      match).
- v1 supports `/` as the separator only; alternate separators
  (`:s#old#new#g`) are deferred.
- v1 is literal substring, no regex.

### Highlighted matches

The result widget already supports a search-highlight overlay
(see `widgets/results.rs`); the editor doesn't. Add the same
hook to the editor render path.

## Constraints

- The vim layer (`narwhal-vim`) owns mode transitions today —
  search prompt is a third mode-like state. Implement as a
  "submode" of normal so Esc returns there.
- One commit, conventional, long-form.
- `clippy --all-targets -- -D warnings`, `fmt --check`, no
  `unwrap`/`expect` in production.

## Concrete steps

### Step 1: SearchState

```rust
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub needle: String,
    pub direction: SearchDirection,
    pub prompt_open: bool,
    pub saved_cursor: Option<(usize, usize)>,
    pub matches: Vec<(usize, usize)>, // (line, byte_col)
    pub current: Option<usize>, // index into matches
    pub highlight: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchDirection {
    #[default]
    Forward,
    Backward,
}
```

`SearchState` lives on each `Tab` (each tab has its own editor and
own search state).

### Step 2: `/` and `?` keybindings

`narwhal-vim`'s normal-mode handler currently doesn't know about
`/`. Add two new actions:

```rust
Action::OpenSearch(SearchDirection),
```

`apply_action` in `core.rs`:

```rust
Action::OpenSearch(dir) => {
    let tab = &mut self.tabs[self.active_tab];
    tab.search.saved_cursor = Some(tab.editor.cursor());
    tab.search.direction = dir;
    tab.search.prompt_open = true;
    tab.search.needle.clear();
}
```

### Step 3: prompt key dispatch

When `search.prompt_open`, route keys to a search-prompt handler
*before* the editor:

- char → push to needle, recompute matches, jump to first match
  in `direction` after `saved_cursor`.
- Backspace → pop char, recompute.
- Enter → accept; close prompt, keep highlights, set `current` to
  whatever the cursor sits on.
- Esc → restore cursor, clear matches, close prompt.

### Step 4: n / N

In normal mode, when `search.needle` non-empty:

- `n` → advance `current` by 1 in `direction` (wrap at end).
- `N` → advance in the opposite direction.

Both jump the cursor to the match.

### Step 5: matches computation

```rust
fn find_all(buffer: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() { return Vec::new(); }
    let mut out = Vec::new();
    for (line_idx, line) in buffer.lines().enumerate() {
        let mut start = 0;
        while let Some(pos) = line[start..].find(needle) {
            out.push((line_idx, start + pos));
            start += pos + needle.len().max(1);
        }
    }
    out
}
```

Recomputed on every prompt keystroke (cheap for typical query
sizes; if profiling later shows it's hot, switch to incremental
matching).

### Step 6: highlight render

In `widgets/editor.rs`, when rendering each line, walk the
`matches` slice and apply the highlight style to the byte range
of each match on that line. The current match (where the cursor
sits) gets `theme.accent` bg; the others get `theme.muted` bg.

### Step 7: substitute parser

Extend the `:` command parser (`commands.rs`) with:

```rust
Command::Substitute {
    range: SubstituteRange, // CurrentLine | WholeBuffer
    pattern: String,
    replacement: String,
    global: bool,
    confirm: bool,
}
```

Parser:

```text
:s/<pat>/<rep>/         → CurrentLine, global=false
:s/<pat>/<rep>/g        → CurrentLine, global=true
:%s/<pat>/<rep>/g       → WholeBuffer, global=true
:%s/<pat>/<rep>/gc      → WholeBuffer, global=true, confirm=true
```

### Step 8: apply substitution

For CurrentLine: take the current line text, replace; if global,
replace all occurrences, else only the first. Write back.

For WholeBuffer with `global=true`: take all lines, replace one
by one.

For `confirm=true`: open a confirmation submode at each match
(y/n/a/q). v1 implements without confirm if confirm requires too
much state machinery; mark the `c` flag as TODO with a status
message.

### Step 9: tests

`tests/editor_search.rs`:

1. `forward_search_finds_first_match`
2. `backward_search_finds_first_match`
3. `n_repeats_search_forward`
4. `capital_n_repeats_in_opposite_direction`
5. `esc_during_prompt_restores_cursor`
6. `enter_during_prompt_keeps_match_highlighted`
7. `substitute_current_line_no_g`
8. `substitute_current_line_g`
9. `substitute_whole_buffer`
10. `substitute_no_match_status_message`

Acceptance: test count rises by **10**.

## Files

- `crates/narwhal-vim/src/mode.rs` (Action::OpenSearch added)
- `crates/narwhal-app/src/core.rs` (SearchState, prompt dispatch,
  n/N, substitute execution)
- `crates/narwhal-app/src/commands.rs` (Substitute parser)
- `crates/narwhal-tui/src/widgets/editor.rs` (highlight render)
- `crates/narwhal-tui/src/layout.rs` (search prompt line)
- `crates/narwhal-app/tests/editor_search.rs` (new)

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` reports +10 from
  baseline.
- Manual smoke: `/users` in a SELECT highlights every `users` in
  the buffer, `n` jumps, `:%s/users/accounts/g` renames all.

## Commit message template

```
feat(editor): vim-style / search and :s substitute

The vim layer shipped without /, ?, n, N, or :s — the user
reaching for muscle memory to rename a few column references hit
nothing and had to retype the buffer. Close the gap with the
documented subset of vim's search/replace.

## /, ?, n, N

- /<pat> opens the forward search prompt; cursor returns on Esc,
  jumps on Enter. ? is the backward variant.
- Once a needle is set, n repeats the search in the original
  direction, N in the reverse.
- All matches highlight with the theme's accent background while
  the search is active; the current match (cursor) is brighter.
- Search is literal substring — regex is a follow-up.

## :s

- :s/<pat>/<rep>/        replace first match on current line
- :s/<pat>/<rep>/g       replace every match on current line
- :%s/<pat>/<rep>/g      replace every match in the whole buffer
- :%s/<pat>/<rep>/gc     same with confirm (status messages
                         walk the matches one by one)

v1 supports / as the only separator and literal substring
patterns. Status message reports the count of replacements made.

SearchState lives per-tab so each editor pane carries its own
needle and highlight state.

Ten new tests cover the search and substitute paths.
```
