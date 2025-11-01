# Plan 06-03 — Context-aware completion

## Why

Today's `gather()` walks the union of keywords + tables + phrases
against a single prefix string. Type `SELECT * FROM u` and you get
`UPDATE`, `USE`, `UNION`, *and* `users` mixed together; DataGrip
shows just `users` because it knows the previous token was `FROM`.

This plan adds a small context detector that walks the editor
buffer backward from the cursor and tells the completion provider
which universe of candidates to prefer.

## Scope

Three contexts, in order of specificity:

- **TableExpected**  — previous token is `FROM`, `JOIN`, `INTO`,
  `UPDATE`, `TABLE`, `DESCRIBE`, `DESC`, `EXPLAIN ... FROM`, or
  the SQL keyword forms of "the next thing should be a table
  name".
- **ColumnExpected(table)** — previous "tokens" form
  `<identifier>.` with the cursor sitting right after the dot.
  Suggest only the columns of that table (already in
  `TableSchema::columns` on the session schema cache).
- **Generic** — everything else; reproduces today's behaviour.

## Constraints

- The context walk must stop at the previous `;` so a statement
  later in the buffer doesn't see an earlier statement's FROM
  clause.
- Multi-statement buffers stay correct.
- Manual Tab / Ctrl-Space and auto-trigger both go through the
  context-aware path.
- One commit, conventional, long-form.
- `clippy --all-targets -- -D warnings`, `fmt --check`, no
  `unwrap`/`expect` in production.

## Concrete steps

### Step 1: Context enum and detector

In `completion.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionContext {
    Generic,
    TableExpected,
    ColumnExpected { table: String },
}

/// Walk backward from `cursor` over `buffer`, stopping at `;` or
/// the start, and decide which context is in play.
pub fn detect_context(buffer: &str, cursor_byte_offset: usize) -> CompletionContext {
    // 1. Trim back to the previous statement boundary.
    // 2. Tokenise the trimmed slice (cheap word + dot scanner).
    // 3. If the last non-whitespace token before cursor is `.`
    //    and the token before that looks like an identifier,
    //    return ColumnExpected { table: that-identifier }.
    // 4. Else if the last "real" keyword token is in the
    //    table-expected set, return TableExpected.
    // 5. Otherwise return Generic.
}
```

The tokeniser doesn't need to handle every SQL nuance — only:

- whitespace skip
- `;` terminator (return immediately if encountered)
- string literals (`'...'`, `"..."`) skipped
- `--` line comments
- `/* */` block comments
- bare identifier vs. keyword (case-insensitive match against the
  table-expected set)
- `.` as a standalone token between two identifiers

### Step 2: extend the editor buffer with a byte-offset cursor accessor

`narwhal-tui::widgets::editor::EditorBuffer` already tracks cursor
position in (line, col). Add `pub fn cursor_byte_offset(&self) ->
usize` that returns the flattened byte offset into the entire
buffer text.

### Step 3: thread the context through gather()

```rust
pub fn gather(
    prefix: &str,
    schemas: &[SchemaListing],
    context: &CompletionContext,
    limit: usize,
) -> Vec<Completion> { ... }
```

Inside `gather`, before the keyword loop:

```rust
match context {
    CompletionContext::TableExpected => {
        // Tables first; keywords last (still useful as a fallback
        // when the user types "FROM SELECT").
        gather_tables(prefix, schemas, &mut prefix_hits, ...);
        gather_keywords(prefix, &mut substr_hits, ...);
    }
    CompletionContext::ColumnExpected { table } => {
        gather_columns_of(prefix, schemas, table, &mut prefix_hits, ...);
        // No keywords here — they don't compose with `t.` syntax.
    }
    CompletionContext::Generic => {
        gather_keywords(...);
        gather_phrases(...);
        gather_tables(prefix, schemas, ...);
    }
}
```

### Step 4: wire context detection into core.rs

`trigger_completion` and `maybe_auto_complete`:

```rust
let buffer_text = self.tabs[self.active_tab].editor.entire_text();
let offset = self.tabs[self.active_tab].editor.cursor_byte_offset();
let context = detect_context(&buffer_text, offset);
let items = gather_completions(&prefix, schemas, &context, 50);
```

### Step 5: column completion needs columns

`SchemaListing` is `(Schema, Vec<Table>)` today. `Table` already
has `columns: Vec<ColumnHeader>` populated when narwhal-app
enriches the schema. Verify the sidebar schema fetch path populates
it; if not, extend it.

## Files

- `crates/narwhal-app/src/completion.rs` (Context, detect, gather)
- `crates/narwhal-app/src/core.rs` (thread context into both
  completion call sites)
- `crates/narwhal-tui/src/widgets/editor.rs` (cursor_byte_offset)
- `crates/narwhal-app/tests/completion.rs` (new tests)

## Tests

Add to `tests/completion.rs`:

1. `from_keyword_narrows_to_tables`: `SELECT * FROM u` + Ctrl-Space
   → only `users`, not `UPDATE` or `UNION`.
2. `dotted_identifier_suggests_columns`: schema with table `users`
   and columns `id`, `name`, `email`; type `users.` + Ctrl-Space →
   `id`, `name`, `email`.
3. `context_stops_at_previous_semicolon`: `SELECT * FROM users; SELECT u`
   + Ctrl-Space at end → Generic, *not* TableExpected (because the
   FROM is past the `;`).
4. `join_keyword_narrows_to_tables`: same as 1 but with `JOIN`.
5. `update_keyword_narrows_to_tables`: same as 1 but with `UPDATE`.

Acceptance: test count rises by **5**.

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` reports +5 from baseline.
- Manual smoke: `SELECT * FROM u` shows only `users` etc.

## Commit message template

```
feat(completion): context-aware suggestions for FROM / JOIN / t.col

Today's gather() returned the union of keywords + tables for any
prefix, so `SELECT * FROM u` mixed `UPDATE`, `UNION`, `USE` and
the actual `users` table together. DataGrip narrows the candidate
set based on the preceding token; this commit does the same.

New CompletionContext enum:

- TableExpected         — previous keyword is FROM / JOIN / INTO /
                          UPDATE / TABLE / DESCRIBE / DESC; the
                          gather call returns tables first,
                          keywords as a tail fallback.
- ColumnExpected{table} — cursor sits right after `ident.`, so
                          the gather call returns only that
                          table's columns.
- Generic               — every other position; today's behaviour.

The detector tokenises the editor buffer backward from the cursor,
stopping at the previous `;` so multi-statement buffers don't
leak context across statements. It skips string literals and SQL
comments so they can't fake a keyword.

Both manual Tab / Ctrl-Space and the auto-trigger after each
keystroke go through the context-aware path.

Five new tests cover each branch + the semicolon boundary.
```
