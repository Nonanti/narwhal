# Plan 07-07 — Saved queries / bookmarks

## Why

History (06-05) is a flat journal of every statement the user
ever ran. "Save this exact query under a name" is a different
need: a small hand-curated library of queries the user reaches
for often — "active users last 30 days", "stale carts", "today's
errors". DataGrip has Saved Queries; DBeaver has Bookmarks.

## Scope

- `:save <name>` — write the current editor buffer to
  `~/.config/narwhal/snippets/<name>.sql`. Overwrites if the
  name already exists (status message confirms).
- `:snippets` — open a modal list of saved snippets. Up / Down /
  j / k navigate, Enter loads the selected snippet into a new
  editor tab. Esc dismisses.
- `:load <name>` — direct load into a new tab without opening
  the modal. Tab-completion (06-09) extends to cover the
  snippets universe for both `:load` and `:save`.
- `:rm-snippet <name>` — delete a snippet file.

Snippet name validation: lowercase letters, digits, `-`, `_`.
Reject anything else with a status message so the snippets
directory stays portable across filesystems.

## Constraints

- AGENTS.md: no `unwrap()` / `expect()` in production code.
- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- One conventional commit, long-form.
- Snippet directory is created lazily on first save.

## Concrete steps

### Step 1: SnippetStore

`crates/narwhal-app/src/snippets.rs` (new):

```rust
pub struct SnippetStore {
    pub root: PathBuf,
}

impl SnippetStore {
    pub fn new(root: PathBuf) -> Self { Self { root } }

    pub fn save(&self, name: &str, sql: &str) -> Result<()> {
        Self::validate_name(name)?;
        std::fs::create_dir_all(&self.root)?;
        let path = self.root.join(format!("{name}.sql"));
        std::fs::write(path, sql)?;
        Ok(())
    }

    pub fn load(&self, name: &str) -> Result<String> {
        Self::validate_name(name)?;
        let path = self.root.join(format!("{name}.sql"));
        Ok(std::fs::read_to_string(path)?)
    }

    pub fn remove(&self, name: &str) -> Result<()> { ... }

    pub fn list(&self) -> Result<Vec<String>> {
        // Read root, filter to `.sql`, strip extension, sort.
    }

    fn validate_name(name: &str) -> Result<()> {
        let ok = !name.is_empty()
            && name.chars().all(|c| c.is_ascii_lowercase()
                                   || c.is_ascii_digit()
                                   || c == '-'
                                   || c == '_');
        if ok { Ok(()) } else {
            Err(Error::InvalidSnippetName(name.into()))
        }
    }
}
```

### Step 2: command parsing

`commands.rs`:
```rust
Command::SaveSnippet { name: String },
Command::LoadSnippet { name: String },
Command::RemoveSnippet { name: String },
Command::ListSnippets,
```

Builtin names + descriptions added to 06-08's CHEATSHEET and the
06-09 tab-complete universe extended for `:load` / `:save` /
`:rm-snippet`.

### Step 3: SnippetsModal

```rust
pub struct SnippetsModal {
    pub entries: Vec<String>,
    pub selected: usize,
}

// On AppCore:
pub snippets_modal: Option<SnippetsModal>,
```

Open: list the store, init `entries` + `selected: 0`. Render as
a centred modal (mirrors 06-05 history modal layout).

Navigation: Up/Down/j/k cycle. Enter loads the selected snippet
into a new tab. Esc dismisses.

### Step 4: render

`crates/narwhal-tui/src/widgets/snippets.rs` (new) — mirrors the
06-05 history modal structure: centred Rect, Block border with
title `snippets · <N> total`, List widget showing one snippet
name per row.

### Step 5: tests

`tests/snippets.rs`:

1. `save_then_load_round_trip` — save "foo" with body "SELECT 1",
   call SnippetStore::load("foo"), assert returns "SELECT 1".
2. `invalid_name_rejected` — try to save "Has Space", assert
   error.
3. `list_returns_sorted_names` — save "b", "a", "c"; list
   returns ["a", "b", "c"].
4. `remove_deletes_file` — save then remove; load → error.
5. `tab_complete_includes_snippets` — save "users-active";
   buffer `:load us`, Tab → `:load users-active` (extends
   06-09 universe).

Acceptance: +5 tests.

## Files

- `crates/narwhal-app/src/snippets.rs` (new)
- `crates/narwhal-app/src/lib.rs` (re-export)
- `crates/narwhal-app/src/commands.rs` (4 new variants)
- `crates/narwhal-app/src/core.rs` (SnippetStore field +
  command dispatch + SnippetsModal state + 06-09 universe
  extension)
- `crates/narwhal-tui/src/widgets/snippets.rs` (new)
- `crates/narwhal-tui/src/widgets.rs` (re-export)
- `crates/narwhal-tui/src/lib.rs` (re-export)
- `crates/narwhal-tui/src/layout.rs` (overlay when modal open)
- `crates/narwhal-tui/src/widgets/help.rs` (CHEATSHEET entries)
- `crates/narwhal-app/tests/snippets.rs` (new)

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean
- `nix develop --command cargo test --all` reports +5 from baseline
- Manual smoke: write a query, `:save active-users`, restart
  narwhal, `:snippets`, Enter on `active-users` — query loads
  into a new tab.

## Commit message template

```
feat(app): saved queries / snippets library

History (plan 06-05) is a journal of every statement ever run.
Saved queries are different: a small hand-curated library of the
queries you reach for often — active-users-30d, stale-carts,
today-errors.

A new SnippetStore at ~/.config/narwhal/snippets persists each
saved query as a separate <name>.sql file.  Names are validated:
lowercase letters, digits, dashes, underscores only, so the
directory stays portable across filesystems.

Four new commands:

- :save <name>        write the current editor buffer
- :load <name>        load a snippet into a fresh editor tab
- :rm-snippet <name>  delete a snippet file
- :snippets           open a modal list, Enter to load

Tab-completion from plan 06-09 extends to include the snippets
universe for :load, :save, and :rm-snippet, so the user only ever
has to type a few characters.

The snippets modal mirrors the 06-05 history modal layout:
centred Rect, List widget, j/k or Up/Down navigation, Enter
loads, Esc dismisses.  All four entries land in the 06-08
CHEATSHEET so F1 documents them.

Five new tests cover the round-trip, name validation, sorted
listing, removal, and the tab-complete extension.
```
