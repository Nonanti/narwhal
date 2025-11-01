# Plan 06-07 — Auto-pair brackets and quotes

## Why

Every modern code editor closes `(` / `'` / `"` / `[` / `{` when
you open one. Not doing it in insert mode is a constant papercut
when writing SQL — `WHERE x IN (...)` requires four keypresses
that the editor could collapse to one.

## Scope

In insert mode only:

- Typing `(` inserts `()` and places the cursor between them.
- Same for `[`, `{`, `'`, `"`, ` ``` `.
- Typing the closing character with the cursor already on the
  matching closer (i.e. the editor inserted it auto-pair-style)
  *skips over* it instead of inserting a duplicate.
- Backspace on an empty pair `(|)` deletes both characters.
- Smart skipping: if the next character is already a closing
  bracket and the user types the matching opener, no auto-pair
  (prevents the `((` → `(()) (`  case from over-pairing).
- Auto-pair is disabled inside string literals (so typing `'`
  inside a string doesn't insert `''`). Detect via "the cursor
  is inside an unbalanced `'` or `"` on the current line".

## Constraints

- Behaviour-preserving when the user types characters with no
  pair semantics (everything else stays the same).
- Single-character undo: pressing the closing char to skip over
  it should still register as a typed-character event so vim's
  `u` / `.` repeat works.
- One commit, conventional, long-form.
- `clippy --all-targets -- -D warnings`, `fmt --check`, no
  `unwrap`/`expect` in production.

## Concrete steps

### Step 1: pair table

```rust
const PAIRS: &[(char, char)] = &[
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('\'', '\''),
    ('"', '"'),
    ('`', '`'),
];
```

### Step 2: EditorBuffer::insert_char hook

In `narwhal-tui::widgets::editor::EditorBuffer::insert_char`:

```rust
pub fn insert_char(&mut self, c: char) {
    if !self.auto_pair_enabled {
        return self.raw_insert_char(c);
    }
    // Handle skip-over closer.
    if let Some((_, close)) = PAIRS.iter().find(|p| p.1 == c) {
        if self.next_char() == Some(*close) && self.last_typed_was_opener_for(*close) {
            self.move_cursor_right();
            return;
        }
    }
    // Handle opener with auto-pair.
    if let Some((_open, close)) = PAIRS.iter().find(|p| p.0 == c) {
        if self.should_auto_pair(c) {
            self.raw_insert_char(c);
            self.raw_insert_char(*close);
            self.move_cursor_left();
            return;
        }
    }
    self.raw_insert_char(c);
}

fn should_auto_pair(&self, opener: char) -> bool {
    // No auto-pair if cursor is inside a string literal.
    if self.cursor_inside_string_literal() { return false; }
    // No auto-pair if the next char is itself an opener that
    // already has its pair just past it (i.e. don't over-pair).
    if let Some(next) = self.next_char() {
        if PAIRS.iter().any(|(o, _)| *o == next) { return false; }
    }
    true
}
```

### Step 3: cursor_inside_string_literal

Walk the current line from column 0 to the cursor, tracking
"inside `'`" and "inside `"`" toggles. On reaching the cursor,
return true if either flag is set.

This is a per-line check; multi-line strings are out of scope for
v1 (SQL doesn't usually have them anyway).

### Step 4: backspace deletes the pair

In `EditorBuffer::backspace`:

```rust
let prev = self.prev_char();
let next = self.next_char();
if let (Some(p), Some(n)) = (prev, next) {
    if PAIRS.iter().any(|(o, c)| *o == p && *c == n) {
        // Delete both characters.
        self.delete_prev_char();
        self.delete_next_char();
        return;
    }
}
self.delete_prev_char();
```

### Step 5: setting + tests

`EditorBuffer` gains:

```rust
pub fn set_auto_pair_enabled(&mut self, on: bool) { ... }
pub fn auto_pair_enabled(&self) -> bool { ... }
```

Defaulted to `true`. Tests need a way to disable for buffer-only
tests that don't want to think about auto-pair.

## Files

- `crates/narwhal-tui/src/widgets/editor.rs`
  (PAIRS, insert_char wrapper, should_auto_pair, backspace pair
  handling, cursor_inside_string_literal, set_auto_pair_enabled)
- `crates/narwhal-app/tests/auto_pair.rs` (new)

## Tests

`tests/auto_pair.rs`:

1. `open_paren_inserts_matched_pair`: insert `(`, assert buffer
   has `()` and cursor is between them.
2. `close_paren_skips_existing_close`: state `()` cursor between,
   insert `)`, assert buffer still `()` and cursor moved right.
3. `quotes_pair`: insert `'`, assert `''` with cursor between.
4. `backspace_inside_empty_pair_deletes_both`: state `()` cursor
   between, backspace, assert buffer empty.
5. `no_pair_inside_string_literal`: state `'where x ='` cursor at
   end-of-string, insert `(`, assert single `(` not `()`.
6. `no_pair_when_next_char_is_opener`: state `()` cursor before
   `(`, insert `(`, assert buffer is `(()` not `(())`.
7. `non_pair_characters_unaffected`: insert `s`, then `e`, then
   `l`, assert buffer is `sel`.
8. `nested_pairs`: insert `(`, then `(`, then `)`, then `)`,
   assert buffer is `(())` and cursor at end.

Acceptance: test count rises by **8**.

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` reports +8 from baseline.
- Manual smoke: typing `WHERE x IN (` produces `WHERE x IN ()`
  with cursor before `)`.

## Commit message template

```
feat(editor): auto-pair brackets and quotes in insert mode

Typing ( | [ | { | ' | " | \\` in insert mode now inserts the
matching closer and parks the cursor between the two. Typing the
closer when the cursor already sits on an auto-pair-inserted
match skips over it instead of duplicating. Backspace on an empty
pair (|) deletes both characters in one keystroke.

Auto-pair is suppressed inside string literals (the cursor's
position relative to unbalanced ' or " on the current line) so
typing ' inside a string doesn't open a runaway ''. Suppressed
again when the next character is itself an opener so deep nesting
doesn't over-pair into (()) ( cases.

A new public set_auto_pair_enabled() on EditorBuffer lets tests
opt out when they want to drive the raw insert path directly.

Eight new tests cover the open/skip/backspace/no-pair-in-string/
nesting branches.
```
