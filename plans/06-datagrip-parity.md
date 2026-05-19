# Plan 06 — DataGrip parity roadmap

## Goal

Close the gap between narwhal and DataGrip on the features that
matter for daily use. The list below was assembled after a manual
smoke test pinpointed where the current TUI feels rough.

The roadmap is two tiers: the **short-term** tier is the minimum
viable polish to make narwhal "DataGrip-like" for a single
developer working against one or two databases. The **long-term**
tier covers everything DataGrip ships that narwhal doesn't, in
descending order of daily-use ROI.

Each work item below gets its own detailed plan file (`plans/06-XX-*.md`)
when the design is locked, so this document is the index and
prioritisation rationale; the per-item plans carry the concrete
steps, file lists, tests, and acceptance criteria.

## Short-term: ~1-2 days of focused work

Implementation order matches dependency order — earlier items
don't require later ones, so each can land independently.

### 06-01. Status bar split

**Why.** The single-line status bar mashes mode, focus, connection,
and the last message together. Long messages truncate, transient
messages overwrite persistent state, and the user has nowhere to
look for the current connection at a glance.

**Scope.** Three slots:

- left:   mode (NOR/INS/CMD) + focused pane label
- center: connection name + driver (sticky)
- right:  last message (transient, fades after N seconds or stays
          until next event)

Optional fourth slot for transaction state when a transaction is
open ("TX · serializable" or similar).

**Acceptance.** Existing TUI snapshots re-recorded. New unit test
asserts the three slots render independently.

**Estimate.** S (~150 LOC).

---

### 06-02. Mouse support

**Why.** Modern terminals support mouse; refusing them is friction
the user shouldn't have to absorb.

**Scope.**

- Crossterm: enable mouse capture in raw mode.
- Click on a pane → focus changes to that pane.
- Click on a sidebar table → "SELECT * FROM <table> LIMIT 100"
  injected into the editor + previewed.
- Click on a completion popup item → accept that item.
- Scroll wheel on the result grid → vertical scroll.
- Scroll wheel on the editor → cursor-aware scroll.
- Click on a result row/cell → select that row/cell.
- Click on a result column header → trigger sort cycle (depends on
  06-04).

**Acceptance.** New `tests/mouse.rs` exercising every mouse-routed
action through the public API. No regressions in existing tests.

**Estimate.** M (~400 LOC, mostly translation of MouseEvent to the
existing handle\_key-style routing).

---

### 06-03. Context-aware completion

**Why.** Current completion is "match prefix against the union of
keywords + tables + phrases". DataGrip narrows the candidate set
based on the token *before* the cursor: `FROM` / `JOIN` / `INTO` /
`UPDATE` produce table-only suggestions; `table.` produces column
suggestions for that table.

**Scope.**

- New `Context` enum: TableExpected, ColumnExpected(table), Generic.
- `current_word_prefix_with_context()` walks the editor buffer
  backward from the cursor and returns `(prefix, context)`.
- `gather()` gains a `Context` argument and prefers:
  - TableExpected → only tables (keywords appear at the bottom of
    the list as a fallback so users typing `FROM SELECT` still
    see something useful).
  - ColumnExpected(t) → only columns of `t` (requires schema
    metadata to include columns, which it already does via
    `narwhal_core::TableSchema::columns`; the result view just
    doesn't show them in the sidebar yet).
  - Generic → today's behaviour.

**Acceptance.** Three new tests in `tests/completion.rs` covering
each context branch. Existing tests still pass because Generic is
the default and reproduces today's order.

**Estimate.** M (~250 LOC + tests).

---

### 06-04. Result sort + filter

**Why.** Looking at a 100-row result and not being able to sort it
without typing `ORDER BY` is a daily friction.

**Scope.**

- Column header click (mouse) or `s` on the focused column (keyboard)
  cycles `None → Asc → Desc → None`. The sort is applied to the
  *current* result snapshot, in memory — narwhal does not re-issue
  the SQL.
- `/` in the result pane opens a small filter prompt at the bottom
  of the result widget. Typing filters the visible rows by
  case-insensitive substring across all columns. Esc closes the
  filter.
- Filter + sort compose: sort orders the filtered subset.

**Acceptance.** New `tests/result_sort_filter.rs` exercising both
keyboard and (after 06-02) mouse paths.

**Estimate.** M (~300 LOC + tests).

---

### 06-05. Query history with `Ctrl+R` search

**Why.** narwhal already writes every executed statement to
`~/.local/share/narwhal/history.jsonl`. The data is there; the UI
to browse it isn't.

**Scope.**

- `Ctrl+R` (or `:history`) opens a modal listing the most recent
  N=200 statements, newest first, with timestamp + connection
  name.
- Typing a substring filters the list (case-insensitive,
  fzf-vari subsequence match is the stretch goal; substring is
  fine for v1).
- Enter inserts the selected statement into the editor; Shift-Enter
  inserts and runs immediately.
- Esc dismisses.

**Acceptance.** New `tests/history.rs` covering open / filter /
accept / dismiss.

**Estimate.** M (~400 LOC; the modal widget is the largest piece).

---

### 06-06. Editor find / replace

**Why.** Vim ships `/`, `?`, `:s/foo/bar/g` and narwhal's vim layer
doesn't implement any of them. Users who type a complex query and
then need to rename a column reference reach for muscle memory
that fails.

**Scope.**

- `/` in normal mode → forward search prompt at the bottom of the
  editor. Matches highlighted; `n` / `N` navigate; Enter exits
  search keeping the cursor on the current match.
- `?` → backward search.
- `:s/old/new/` → replace on current line.
- `:s/old/new/g` → replace on current line, all occurrences.
- `:%s/old/new/g` → replace in the whole buffer.

No regex in v1: literal substring matching. Adding regex is a
stretch goal once the UX is solid.

**Acceptance.** New `tests/editor_search.rs` covering forward,
backward, `n` / `N` navigation, and the three `:s` variants.

**Estimate.** M (~300 LOC + tests).

---

### 06-07. Auto-pair brackets and quotes

**Why.** Every modern code editor closes `(` / `'` / `"` / `[` / `{`
when you open one. Not doing it in insert mode is a constant
papercut.

**Scope.**

- In insert mode, typing `(` inserts `()` and places the cursor
  between them.
- Same for `[`, `{`, `'`, `"`, and ``` ` ```.
- Typing the closing character with the cursor already on it
  skips over it instead of inserting a duplicate.
- Backspace on an empty pair `(|)` deletes both characters.
- Smart skipping: if the next character is already a closing
  bracket and the user types the matching opener, no auto-pair
  (prevents the `((` case from doubling unnecessarily).

**Acceptance.** New `tests/auto_pair.rs` covering each rule.

**Estimate.** S (~150 LOC + tests).

---

### 06-08. Help panel (`?` / F1)

**Why.** narwhal's keymap is now wide enough that nobody can
remember it. DataGrip has "Help → Keyboard Shortcuts PDF"; we can
do better with a live cheatsheet panel.

**Scope.**

- `?` in normal mode or `F1` anywhere opens a centred modal
  listing every keybinding in three columns: editor, results,
  global.
- Mouse-clicking outside or pressing Esc / `?` / F1 closes it.
- Static content — no need to introspect from the keymap struct
  for v1.

**Acceptance.** Snapshot test for the modal.

**Estimate.** S (~150 LOC).

---

### 06-09. `:` prompt tab-completion

**Why.** `:open <name>`, `:remove <name>`, `:forget <name>` all
take connection names. Typing them out is tedious; `:` should
tab-complete against the relevant universe.

**Scope.**

- When the prompt buffer matches `:open `, `:remove `, `:forget `,
  Tab completes against connection names from the loaded
  `ConnectionsFile`.
- When it matches `:help `, Tab completes against
  `BUILTIN_COMMAND_NAMES` (already used for autocomplete in the
  editor) plus plugin-registered commands.
- When it matches `:export `, Tab completes against the format
  list (`csv`, `json`).

**Acceptance.** New `tests/prompt_completion.rs`.

**Estimate.** S (~100 LOC + tests).

## Long-term: weeks of work

These are the remaining DataGrip features in descending order of
daily-use ROI. Each becomes a separate plan when the short-term
tier lands.

### 06-10. Multi-cursor edit

Visual-block-mode → column edit, plus explicit Ctrl-click in
insert mode to add a secondary cursor. The hardest part is making
the vim layer multi-cursor-aware without breaking the existing
single-cursor model.

**Estimate.** L (~1 week).

### 06-11. SQL formatter

Inline `:format` or `=` on a visual selection runs the SQL through
a deterministic pretty-printer (uppercased keywords, indented
clauses, aligned column lists). The hard part is the parser —
narwhal currently has no SQL AST.

**Estimate.** L (~1 week, plus the build-out of a minimal SQL
tokeniser/parser).

### 06-12. ER diagram

ASCII rendering of foreign-key graph in a popup. Schema metadata
already carries FK info; only the layout algorithm is new.

**Estimate.** M-L (~1 week, mostly the layout heuristic).

### 06-13. Plot view

Numeric result columns → ASCII bar/line chart in a result-replacing
view. Useful for ad-hoc analytics.

**Estimate.** M (~3 days).

### 06-14. Query profiler

`EXPLAIN ANALYZE` parser → annotated cost tree, with the hottest
nodes highlighted. Driver-specific (Postgres + ClickHouse first).

**Estimate.** L (~1 week per driver).

### 06-15. Diff view

Two result sets side-by-side, columnar diff highlighting. Useful
for "before vs. after" comparisons.

**Estimate.** M (~3 days).

## Implementation strategy

The short-term tier can be parallelised across subagents because
the dependency graph is mostly flat:

```
06-01 status bar    ─┐
06-02 mouse        ──┼─→ (already independent)
06-07 auto-pair    ─┘
06-08 help panel   ─→ (independent)
06-09 : tab-comp   ─→ (independent)

06-04 sort/filter ─→ depends on 06-02 (mouse) for click-on-header,
                     but keyboard path lands first

06-03 context-completion ─→ touches the same completion.rs / core.rs
                            as 06-04's filter prompt — sequential
                            with 06-04 to avoid merge pain

06-05 history Ctrl-R ─→ depends on 06-06 if we want regex search;
                        otherwise independent
06-06 find/replace ─→ touches editor / vim layer — sequential with
                       06-05 to avoid merge pain
```

Realistic parallel batch (each = one rust-dev subagent):

- Batch A: 06-01, 06-02, 06-07, 06-08, 06-09 (5 independent items)
- Batch B: 06-03 (context completion) + 06-04 (sort/filter) sequentially
           on a separate branch
- Batch C: 06-05 (history) + 06-06 (find/replace) sequentially

After each batch a manual review + bug-fix pass on my side, just
like the previous turns.

## Risks

- **Mouse + TUI focus model**: ratatui doesn't track hit-regions
  natively. Each draw call will need to remember the screen
  rectangle of each clickable element so the MouseEvent handler
  can dispatch on coordinates. Implementation is straightforward
  but touches every widget.
- **Context-aware completion + multi-statement buffers**: the
  context walk has to stop at the previous `;` so the cursor in
  statement N doesn't see the FROM clause of statement N-1.
- **Sort/filter on streamed results**: streaming results don't
  have a finite row vector to sort. Plan 06-04 only supports sort
  on materialised (non-streamed) results in v1; streamed results
  fall back to "sort after stream completes".
- **`:s` and friends**: vim's `:s` modifier syntax is complex
  (flags, ranges, separators other than `/`). v1 supports the
  three documented forms only.

## Next step

If this prioritisation looks right, the next action is to expand
each short-term item (06-01 through 06-09) into its own plan file
with the brief that goes to the subagent. After that, batch A
fires in parallel under `worktree=true, concurrency=5`.
