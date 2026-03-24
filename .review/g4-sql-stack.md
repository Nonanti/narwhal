# G4 — SQL Stack Review

**Scope**: `narwhal-sql` (3.2K LOC), `narwhal-schema-diff` (2.7K LOC), `narwhal-commands` (11K LOC)  
**Date**: 2026-06-05  
**Reviewer**: Automated codebase audit, 3-pass review  
**Build**: `cargo check` ✓, `cargo clippy -- -D warnings` ✓, `cargo fmt` ✓, `cargo test` ✓ (all 85 + 5 + 29 + 8 + 13 + 10 = ~150 tests pass)

---

## 1. narwhal-sql

### 1.1 Treesitter Wiring

**Files**: `treesitter/{mod,parser,edit,scope,highlight,tests}.rs`

#### Parser & Incremental Reparse

- `Parser::new_with_grammar()` correctly handles the `tree-sitter-sequel 0.3` `LANGUAGE: LanguageFn` coupling at `parser.rs:54` via `tree_sitter_sequel::LANGUAGE.into()`.
- `Parser::edit()` returns `false` silently when no tree is cached — documented contract, callers must fall back to `parse()`. Fine.
- `Parser::reparse()` falls back to full parse when no old tree exists — correct.
- `Parser` is correctly documented as `!Send` (holds raw C pointers). Each tab owns its own instance on the UI thread. Good boundary.
- `SqlTree` intentionally does not expose `clone()` (tree-sitter `Tree::clone` is expensive). The `raw()` method provides `&Tree` for advanced callers. Correct design.

**Finding T3-1** — `parser.rs:62-63`: After `self.tree = Some(SqlTree { inner: tree })`, the method does `self.tree.as_ref().ok_or(ParseError::NoTree)`. This double-check is redundant since `Some` was just inserted. Harmless but worth a comment or simplification to `Ok(self.tree.as_ref().unwrap())` — the `unwrap` is safe by construction and the pattern is used elsewhere in this codebase (e.g., `push_span` in highlight.rs uses `unwrap_or_else(|| unreachable!())` for similar invariant-guaranteed access).

**Severity**: T4 (style/nit)

#### Edit API

- `Edit::with()` builder pattern is clean, `#[non_exhaustive]` on the struct allows future fields.
- `Edit::from_diff()` correctly computes `(row, col)` via `byte_position()`, which clamps `byte_offset` to `src.len()`. Correct.
- `into_input_edit()` maps cleanly to `tree_sitter::InputEdit`. Correct.

**Finding T3-2** — `edit.rs:99-109`: `byte_position()` walks the source bytes up to `limit` with a linear scan. For very large buffers (>100K lines) this is O(n) per edit. In practice, editor edits are infrequent and the buffer rarely exceeds 10K lines, so this is fine. Documenting the linear cost would be helpful.

**Severity**: T4 (doc)

#### Scope Detection

- `scope_at_offset()` correctly walks from the innermost named node upward, mapping node kinds to `ScopeKind`. The walk-up strategy is sound.
- `offset_is_at_or_after_keyword()` correctly disambiguates `GROUP BY` vs `HAVING` and `JOIN` table vs `ON` condition. Good.
- `classify_insert_list()` correctly distinguishes `InsertColumns` vs `InsertValues` by checking whether `keyword_values` has been seen before the target `list` node. Correct.
- `Scope::none()` is `const fn` — correct for the empty-buffer case.

**Finding T3-3** — `scope.rs:46-51`: `descendant_for_byte_range(byte_offset, byte_offset)` uses a zero-width range. Tree-sitter's documentation states this returns the smallest node that *starts* at or after `byte_offset`, which may not be the node that *contains* the offset. In practice this works because most cursor positions land inside a node, but an offset at the exact boundary between two nodes (e.g., just after a `;`) could return the *next* statement's node instead of `None`. The subsequent `enclosing_statement()` walk then produces a scope for the wrong statement.

**Severity**: T3 (subtle, only at exact token boundaries, minimal UX impact)

#### Highlight Emission

- The walk is loop-based via `TreeCursor`, avoids recursion — correct.
- `classify()` maps node kinds to `HighlightKind` with reasonable coverage of `tree-sitter-sequel` node types.
- `push_span()` enforces no-overlap invariant and trims/removes overlapping spans from malformed input — defensive and correct.
- `identifier_kind()` uses field-name edges and parent/grandparent kinds to distinguish `TableRef`, `FunctionCall`, `ColumnRef`, `Alias`, etc. Reasonable heuristic.

**Finding T3-4** — `highlight.rs:166-172`: `identifier_kind()` only checks `grandparent.kind() == "from"` for `TableRef`, but does not check for `join` parents. An identifier inside `FROM a, b` where `b` is a child of the `from` node (not `object_reference`) would miss the `TableRef` classification. The grammar typically wraps these in `object_reference`, so this may be a non-issue with `tree-sitter-sequel`, but it's worth verifying.

**Severity**: T4 (potential missed highlight, no functional impact)

#### Length-Keyed Cache

No length-keyed cache is present in the current implementation. The `Parser` caches a single `SqlTree` (the most recent parse). There is no cache keyed by source length or content hash. This is fine for the current use case (one tree per buffer, incremental edits), but the task description mentions "length-keyed cache" — this may be a planned feature that hasn't landed yet.

**Severity**: N/A (not implemented, not a bug)

### 1.2 Lint Module

**File**: `lint.rs`

**Strengths**:
- Comment stripping before destructive checks, preserving the original for `select-star` (so `-- lint:allow` pragmas work). Correct.
- `INSERT … SELECT …` is correctly exempted from `select-star`. Good.
- CTE prefix is correctly stripped via `skip_cte_prefix()` before keyword matching. Handles `RECURSIVE`, column lists, `NOT MATERIALIZED` / `MATERIALIZED`, and multi-CTE commas. Comprehensive.
- `find_top_level_word()` is whitespace-insensitive and paren-depth-aware. Correct.
- Dialect-aware splitting via `split_with()` prevents false positives from string-literal semicolons, dollar-quoted strings, etc. Good.

**Finding T3-5** — `lint.rs:89-92`: `check_select_star()` operates on a line-by-line scan of the original source. A multi-line `SELECT *` where `SELECT` and `*` are on different lines will not be caught. This is documented as intentional (the line-based scan preserves `-- lint:allow` pragma visibility), but it means `SELECT\n*` evades the rule.

**Severity**: T4 (documented limitation, false-negative, rare in practice)

**Finding T3-6** — `lint.rs:195-200`: `check_destructive_no_where()` checks for `starts_with("DELETE FROM")` but `DELETE` without `FROM` is valid in some dialects (e.g., `DELETE users WHERE ...` in MySQL). The `starts_with("DELETE ")` arm catches this, but the ordering of checks means `starts_destructive` is true for both forms. No bug, but the `starts_with("DELETE FROM")` arm is redundant since `starts_with("DELETE ")` already covers it.

**Severity**: T4 (dead code / redundancy)

**Finding T3-7** — `lint.rs:272-280`: `check_cartesian_join()` correctly uses `find_top_level_word()` for `WHERE`, `GROUP`, `ORDER`, `LIMIT` detection. However, it does not check for `HAVING` — a query with `FROM a, b HAVING COUNT(*) > 1` but no `WHERE` or `JOIN` would still fire the cartesian-join rule, which is correct (HAVING doesn't join the tables). Good.

**Severity**: N/A (correct behavior)

### 1.3 Guard / Statement Classification

**File**: `guard.rs`

**Strengths**:
- `strip_leading_comments_and_whitespace()` correctly handles nested `--` and `/* */` comments.
- `guard_read_only()` deny-list uses `contains_word()` for word-boundary matching — no false positives on `sleeping_bags`.
- `strip_sql_literals()` masks literal bodies so `'pg_sleep'` can't bypass the scanner.
- Backtick identifiers are intentionally *not* stripped (documented: they're already word-boundary separated, and stripping them would *create* a bypass). Correct.

**Finding T3-8** — `guard.rs:53-56`: `classify_statement()` classifies `WITH ... INSERT` as `Read` because the first token is `WITH`. This is documented behaviour, and the guard notes that `BEGIN`/`ROLLBACK` sandwich in MCP catches it at a deeper layer. However, `COPY` is classified as `Write` (correct for PG `COPY ... FROM STDIN`), but SQLite's `COPY` command doesn't exist. No actual bug since SQLite drivers won't produce `COPY`, but it's a minor classification inaccuracy for engines that don't support it.

**Severity**: T4 (documented, handled at a deeper layer)

**Finding T3-9** — `guard.rs:97-98`: `SET` is classified as `Ddl`, but `SET search_path = public` is a session-level statement that doesn't mutate schema. `SET TRANSACTION` is also `Ddl` here but semantically belongs in `Tx`. The current classification is conservative (treats all `SET` as potentially mutating), which is the safer choice.

**Severity**: T4 (over-classification, safer than the alternative)

### 1.4 Splitter

**File**: `splitter.rs`

**Strengths**:
- State machine correctly handles string literals, dollar-quoted strings, backtick identifiers, block comments (with nesting depth tracking), and E-prefix strings.
- `match_dollar_tag()` correctly recognises `$$` (empty tag) and `$tag$` (named tag) in Postgres mode.
- `find_dollar_close()` uses `memchr::memmem` for performance. Good.
- MySQL backslash escape handling is correct and dialect-gated.
- Postgres `E'...'` prefix is correctly detected only at token boundaries.

**Finding T3-10** — `splitter.rs:210-215`: The `Backtick` state doesn't handle escaped backticks (`` ` `` doubled inside backtick identifiers, which MySQL supports). A backtick identifier like `` `a``b` `` would prematurely close at the second backtick. However, MySQL's backtick escape is actually `` `a``b` `` (doubled), and the current code would see the first closing backtick and exit, treating the third backtick as opening a new identifier. This would cause mis-splitting.

**Severity**: T3 (edge case, only affects MySQL backtick-escaped identifiers with doubled backticks)

### 1.5 Formatter

**File**: `formatter.rs`

- `format()` correctly wraps `sqlformat::format()` with appropriate options.
- Trailing whitespace trimming per line is a good defensive measure.
- `format_for_driver()` maps driver names to dialects. The `dialect` parameter is currently unused (passed to `_`), documented as forward-compatible. Fine.

**Severity**: No issues found.

---

## 2. narwhal-schema-diff

### 2.1 Diff Algorithm

**File**: `diff.rs`

**Strengths**:
- Determinism via `BTreeSet`/`BTreeMap` iteration. Correct.
- System schema filtering uses exact-match (`matches!` with specific strings), not prefix matching. `pg_catalog_clone` survives. Correct.
- Implicit PK index filtering (`i.primary`) correctly skips auto-generated primary key indexes. Correct.
- `referential_action_differs()` normalises `None` and `Some(NoAction)` as equal. Correct.
- `index_by_lower_name()` uses case-insensitive lookup with `tracing::warn` on collision. First-wins policy is deterministic. Correct.

**Finding T2-1** — `diff.rs:70-78`: `is_system_schema()` filters by exact match on a hardcoded list. This misses `pg_temp` (PostgreSQL temporary schema), `INFORMATION_SCHEMA` (SQL Server uppercase form), and `backup` (MSSQL system schema). These could surface phantom diffs against system tables.

**Severity**: T3 (missing system schema entries, phantom diffs against system tables, easily extensible)

**Finding T2-2** — `diff.rs:126-131`: `diff_columns()` uses case-insensitive matching via `index_by_lower_name()`, which means two columns that differ only in case (`"Email"` vs `"email"`) are treated as the same column. In PostgreSQL, these are *distinct* columns. The `tracing::warn` fires, but the diff silently keeps only the first occurrence. This could produce incorrect diffs for PostgreSQL schemas with case-sensitive column names.

**Severity**: T3 (PostgreSQL case-sensitive identifiers are collapsed, with warning but potential data loss in diff output)

**Finding T2-3** — `diff.rs:170-171`: `indexes_differ()` compares columns as `Vec<String>` with `!=`, which means column order matters. This is correct for indexes (`(a, b)` ≠ `(b, a)` for B-tree). However, it doesn't compare other index attributes that might differ (e.g., `where` clause for partial indexes, operator class, sort direction `ASC`/`DESC`). The `Index` struct only has `name`, `columns`, `unique`, `primary` — so this is a schema model limitation, not a diff bug.

**Severity**: T4 (model limitation, not a bug in the diff logic itself)

### 2.2 Type Normalisation

**File**: `normalise.rs`

**Strengths**:
- `canonical_type()` pipeline (trim → lowercase → collapse whitespace → apply synonyms with word-boundary check) is sound.
- Synonym table covers the most common ANSI long forms → short forms.
- Word-boundary check prevents `character_set` from becoming `char_set`. Correct.

**Finding T2-4** — `normalise.rs:35-45`: The synonym table maps `integer` → `int4`, but doesn't map `int` → `int4` (or `int` → `integer`). Some drivers report `int` while others report `integer` or `int4`. Currently `int` stays as `int` and `integer` maps to `int4`, so `int` vs `integer` would produce a phantom diff (`int` ≠ `int4`). Similarly, `bigint` → `int8` but there's no mapping for `bigserial` or `smallserial`. The mapping `smallint` → `int2` is present, which is correct.

**Severity**: T3 (missing `int` → `int4` mapping causes phantom type diffs when drivers disagree on short vs. intermediate vs. long form)

**Finding T2-5** — `normalise.rs:41`: `timestamp without time zone` maps to `timestamp`, but there's no mapping for `timestamp(0) without time zone` or `timestamp(3) with time zone` (precision-qualified forms). The `apply_synonyms()` function does a simple `find` + replace, which would not match `timestamp(0) without time zone` because the `(0)` sits between `timestamp` and `without time zone`. This could cause phantom diffs between drivers that include precision and those that don't.

**Severity**: T3 (precision-qualified timestamp synonyms not handled, phantom diffs on timestamp precision)

**Finding T2-6** — `normalise.rs:82-92`: `defaults_equal()` strips outer parens via `strip_paren_wrap()`, which correctly verifies balanced depth. However, it doesn't strip trailing `::type` casts (e.g., `'foo'::text` vs `'foo'`). In PostgreSQL, the driver may include the cast in the default expression. This would produce a phantom diff.

**Severity**: T3 (PostgreSQL `::type` cast suffix in defaults not stripped, phantom diffs)

### 2.3 Dialect Emitters

#### Generic Emitter

**File**: `emit/generic.rs`

- Correctly emits `-- TODO:` for type changes (ANSI doesn't have `USING`).
- FK/unique/index changes use drop-then-recreate. Correct.

**Finding T4-1** — `emit/generic.rs:139-141`: `render_create_index()` with an empty `table` string produces `CREATE INDEX name ON  (cols)` (double space). This happens for CREATE TABLE follow-up indexes where the table argument is `""`. Minor formatting issue.

**Severity**: T4 (cosmetic, double space in generated DDL)

#### Postgres Emitter

**File**: `emit/postgres.rs`

- `ALTER COLUMN ... TYPE ... USING col::type` is the correct Postgres syntax. Good.
- Drop-then-recreate ordering (FK → UNIQUE → INDEX → columns → INDEX → UNIQUE → FK) is correct and documented. Phase ordering prevents FK conflicts during column type changes.

**Finding T2-7** — `emit/postgres.rs:159-161`: `emit_create_index()` takes `(schema_name, table_name)` but the `table_name` is extracted via `qualified.rsplit('.').next().unwrap_or(qualified)`. This strips the schema from the table name, then re-qualifies it inside `qualify(schema_name, table_name)`. This works correctly for `public.users` → table_name = `users` → `public.users`. However, for tables with dots in their name (e.g., `"my.table"`), the `rsplit('.')` would incorrectly split on the dot inside the quoted identifier. This is a known limitation with unquoted qualified names.

**Severity**: T4 (edge case with dot-containing table names, unlikely in practice)

#### MySQL Emitter

**File**: `emit/mysql.rs`

- MODIFY COLUMN coalescing is correct and well-documented. Type + nullable + default deltas on the same column produce one `MODIFY COLUMN` statement.
- `DROP FOREIGN KEY` syntax is correct (MySQL-specific, not `DROP CONSTRAINT`).
- `DROP INDEX ... ON table` syntax is correct.

**Finding T2-8** — `emit/mysql.rs:138-142`: When only a nullable or default change occurs without a type change, the MODIFY COLUMN statement emits `/* keep existing type */` as a placeholder. This is a comment, not valid SQL syntax — if a user blindly runs the output, MySQL will fail on the comment in the column definition position. The comment should be placed *above* the ALTER TABLE statement instead.

**Severity**: T3 (inline comment produces invalid SQL if executed without review)

#### SQLite Emitter

**File**: `emit/sqlite.rs`

- Correctly identifies operations that require table rebuild (type change, nullable change, default change, FK/unique changes).
- `DROP COLUMN` includes version comment (`requires SQLite >= 3.35`). Good.
- CREATE TABLE includes FKs inline. Correct for SQLite.

**Finding T2-9** — `emit/sqlite.rs:133-137`: The `emit_column_change()` for `DefaultChanged` uses `format!("table rebuild needed: alter default on {table}.{name} (desired: {source:?})")`, which uses `Debug` formatting (`{:?}`) for `source` (an `Option<String>`). This produces `Some("0")` instead of just `0`. The message should use `.as_deref().unwrap_or("NULL")` or similar for cleaner output.

**Severity**: T4 (Debug formatting in user-facing comment, cosmetic)

#### MSSQL Emitter

**File**: `emit/mssql.rs`

- Named default constraints (`df_<table>_<column>`) are correct for T-SQL.
- Default constraints are dropped before column ALTERs and recreated after. Correct.
- ALTER COLUMN coalesces type + nullable. Correct.

**Finding T2-10** — `emit/mssql.rs:130-135`: When a default is being changed and the *target* side has a default (i.e., the old default exists), the code drops the named constraint using `default_constraint_name()`. But if the original default constraint was auto-named by SQL Server (which uses a random hex suffix like `DF__users__email__3B75D7A0`), the synthesised `df_users_email` name won't match, and the `DROP CONSTRAINT` will fail at execution time. The header comment warns about this, but it's still a foot-gun.

**Severity**: T3 (synthesised default constraint name won't match auto-named constraints, documented but still a runtime error)

**Finding T2-11** — `emit/mssql.rs:188-194`: When a `DefaultChanged` has `target: Some(...)` (old default exists) AND `source: Some(...)` (new default desired), the code drops the old constraint and adds a new one. But the drop uses `default_constraint_name(table_name, name)` while the add also uses `default_constraint_name(table_name, name)`. This means the same name is dropped and re-added, which is correct *if* the original constraint was indeed named `df_table_col`. If not, the drop fails silently and the add produces a *second* constraint, leading to ambiguity. The header comment acknowledges this.

**Severity**: T3 (same as T2-10, compounded by double-constraint risk)

### 2.4 Drop-Then-Recreate Ordering

All emitters follow the documented ordering: drop FKs → drop UNIQUE → drop indexes → column changes → recreate indexes → recreate UNIQUE → recreate FKs. This ordering is correct:

1. FKs reference columns, so they must be dropped before column type changes.
2. UNIQUE constraints may reference indexes, so indexes are recreated first.
3. FKs are recreated last because they reference other tables' unique constraints.

**Severity**: No issues found. Ordering is consistent across all emitters.

---

## 3. narwhal-commands

### 3.1 Command Parser

**File**: `commands.rs`

**Strengths**:
- Comprehensive alias system with `BUILTIN_COMMAND_NAMES`, `BUILTIN_COMMAND_DESCRIPTIONS`, and `resolve_builtin_alias()`.
- `:schema-diff` / `:schemadiff` → same parser. Correct.
- `:diff` without args → `Pending`, with args → `DiffSchema`. Smart disambiguation.
- `:export` trailing-flag parser (`split_export_flags`) correctly handles paths with interior spaces by peeling flags from the right.
- `:diagram` subcommand parsing with bare-table escape (`:diagram -- export` for a table literally named `export`).
- Every built-in command has a description entry (verified by test).

**Finding T3-11** — `commands.rs:557-558`: `parse_schema_diff()` uses `positional.pop()` and `positional.pop()` to extract target and source from a Vec guaranteed to have length 2. The `.expect("len == 2")` is safe but reads backwards (the second `.pop()` gets `source`, the first gets `target`). This is correct but could be clearer with destructuring: `let [source, target] = positional.try_into().unwrap()` or similar.

**Severity**: T4 (readability)

**Finding T3-12** — `commands.rs:348-355`: `parse_export()` handles format aliases like `md` → `markdown` and `pq` → `parquet` at the dispatch level, but the `format` field in `Command::Export` is a raw `String`, not an `ExportFormat` enum. This means the dispatch layer must re-parse the format string. An alternative would be to parse it here and store the enum, but the current approach is consistent with the existing pattern and allows future format plugins. Acceptable.

**Severity**: T4 (design choice, not a bug)

**Finding T3-13** — `commands.rs:417-422`: `parse_chart()` uses short flags `-x`, `-y`, `-c`, `-t` alongside long forms `--x`, `--y`, `--col`, `--title`. However, `-t` collides with the common `-t` for `--table` (used in `:diagram`). Since chart and diagram are separate commands, there's no actual conflict, but the inconsistency in flag naming conventions could confuse users.

**Severity**: T4 (UX consistency)

### 3.2 Export Pipeline

**File**: `export/mod.rs`, `export/{csv,json,tsv,table,insert,markdown,parquet}.rs`

#### CSV

- RFC 4180 compliant: CRLF line endings, double-quote escaping, fields quoted when they contain `,`, `"`, `\n`, `\r`, `\t`. Correct.
- NULL renders as empty field — correct per common convention.

**Finding T3-14** — `export/csv.rs`: No UTF-8 BOM is emitted. The task description asks about "UTF-8 BOM?" — the current code does not emit one. This is technically correct per RFC 4180, but some tools (notably Excel on Windows) require a BOM to correctly detect UTF-8 encoding. This is a deliberate design choice (not a bug), but worth documenting.

**Severity**: T4 (design choice, not a bug, common interoperability concern)

#### JSON

- Array-of-objects format. Correct.
- `Value::Bytes` with invalid UTF-8 is emitted as `{"$bytes": "<base64>"}`. Correct round-trip.
- Non-finite floats (`NaN`, `inf`) emit as `null`. Correct (JSON has no representation for these).

**Severity**: No issues found.

#### Parquet (T1-T4-B)

**File**: `export/parquet.rs`

**Strengths**:
- Schema inference from first 100 rows, with widening (`Int64` → `Float64` → `Utf8`). Correct.
- Date/DateTime/Timestamp → `Timestamp(Microsecond, UTC)`. Correct per spec.
- Time → `Utf8` (no portable Arrow type). Correct per spec.
- `ColumnBuilder` enum avoids `Box<dyn ArrayBuilder>` (not object-safe). Correct design.
- Atomic write via `.tmp` + `rename`. Staging file cleaned up on error. Correct.
- `NULL` short-circuits before typed dispatch (avoids E0382 borrow issues). Correct.

**Finding T2-12** — `export/parquet.rs:47-50`: Schema inference scans only the first 100 rows. If row 101 introduces a type not seen in the first 100 (e.g., the first 100 rows have only `Int` values but row 101 has a `Float`), the column is inferred as `Int64` and the float value at row 101 is silently dropped as NULL (via the `(typed, _) => typed.append_null()` fallback at line 234). This data loss is documented as a known limitation, but the user receives no warning.

**Severity**: T2 (silent data loss when type inference is wrong, no warning emitted)

**Finding T2-13** — `export/parquet.rs:178-179`: `LogicalType::widen()` maps `(Timestamp, _)` or `(_, Timestamp)` to `Utf8`. This means a column that contains both a `Timestamp` and a `String` is widened to `Utf8`, which is correct. However, a column with both `Timestamp` and `Int64` values (which can happen if a driver reports the same column differently across rows) would also become `Utf8`, losing type information. This is the intended fallback per the spec ("mixed→Utf8"), but it's worth noting.

**Severity**: T4 (documented design choice)

**Finding T3-15** — `export/parquet.rs:222-225`: `ColumnBuilder::append_value()` handles `Value::Bool` being appended to an `Int64` builder (maps `true` → 1, `false` → 0) and `Int64` being appended to a `Float64` builder (casts with `as f64`). However, `Value::Bool` appended to a `Float64` builder maps `true` → `1.0` and `false` → `0.0` via `f64::from(i32::from(*v))`. This is correct. No issue.

**Finding T3-16** — `export/parquet.rs:82-84`: The staging path is computed as `.<filename>.tmp`. If two concurrent exports target the same file (unlikely but possible in a multi-threaded scenario), they would collide on the same staging path. The atomic rename would then have a race condition. However, since the TUI is single-threaded, this is not a practical concern.

**Severity**: T4 (theoretical race, not possible in current architecture)

#### Markdown (T1-T4-B)

**File**: `export/markdown.rs`

- GFM table format with alignment inference (numeric → right-aligned, else left). Correct.
- Pipe, newline, backslash escaping is correct.
- Row limit truncation with `…N more rows truncated` marker. Correct.
- NULL renders as `(null)`. Correct.
- Empty columns produce `_no result to export_`. Correct.

**Severity**: No issues found.

#### Insert

- Double-quotes all identifiers (reserved-word safe). Correct.
- Schema-qualified table names are properly quoted. Correct.
- Hex blob literals (`X'...'`) for bytes. Correct.
- SQL string escaping (doubled single quotes). Correct.

**Severity**: No issues found.

### 3.3 Streaming Parquet Rejection

**File**: `export/mod.rs:96-99`

- `write_format()` and `write_format_with_options()` correctly reject `Parquet` format with a `Serialise` error explaining that a file path is needed. The error message is clear: "parquet export cannot stream to a generic Write sink — use export_rows with a file path".
- The reject path is clean: early match arm returns `Err`, no unreachable code.

**Severity**: No issues found.

### 3.4 Cell Edit

**File**: `cell_edit.rs`

**Strengths**:
- `parse_input_typed()` uses the column's SQL type hint to avoid misinterpreting `true` as a boolean when the column is `TEXT`. Correct and important.
- `build_update()` correctly rejects updates when no PK exists or when a PK value is NULL. Safety-first.
- Dialect-aware identifier quoting and parameter placeholders (`$1` for PG, `?` for others). Correct.

**Finding T3-17** — `cell_edit.rs:31-38`: There is no undo stack implementation in this module. The module generates a `CompiledUpdate` but does not maintain a history of previous values. The undo stack must live in the host application layer. This is correct — the module is documented as "stateless command and helper modules" — but the task description asks about "undo stack, validation, type coerce, lock". Validation and type coerce are present; undo stack and lock are the host's responsibility.

**Severity**: T4 (architecture, not a bug)

**Finding T3-18** — `cell_edit.rs:111-112`: `is_bool_type()` checks `h == "BOOL" || h == "BOOLEAN" || h.contains("BOOL")`. The `.contains("BOOL")` would match a hypothetical type name like `TINYBOOL` (not a real SQL type) but would also incorrectly match `IS_BOOLEAN` or any type with `BOOL` as a substring. In practice, SQL type names containing `BOOL` are all boolean-ish, so this is acceptable.

**Severity**: T4 (over-broad matching, not a practical issue)

### 3.5 Format (SQL Formatter)

**File**: (via `narwhal-sql::formatter.rs`)

- The formatter wraps `sqlformat` crate with 4-space indent, uppercase keywords, and 2-line separation between queries.
- Comment preservation is best-effort via `sqlformat` (line comments kept, block comments in expressions are best-effort). This is documented.
- Multi-statement split is handled by `sqlformat` itself, which splits on `;` boundaries.

**Severity**: No issues found.

### 3.6 Keymap

**File**: `keymap.rs`

**Strengths**:
- `KeyChord` normalisation (lowercase letters + SHIFT bit) is correct and consistent.
- `parse()` is case-insensitive for modifiers and handles `+`-separated chord syntax.
- Duplicate modifier detection (`ctrl+ctrl+s` → error). Correct.
- `apply_overrides()` returns diagnostics for bad chords/actions rather than panicking. Good.
- Round-trip with `to_string_canonical()` is verified by test.

**Finding T3-19** — `keymap.rs:88-89`: `parse_key_name()` maps `"space"` to `KeyCode::Char(' ')`, but doesn't map `"spc"` or `"spacebar"` which are common alternatives. Minor UX gap.

**Severity**: T4 (missing alias, not a bug)

**Finding T3-20** — `keymap.rs:97-99`: `parse_key_name()` matches single-character input via the `_` arm (`name.chars().count() == 1`). However, a bare `+` character would be interpreted as the key `+`, which could confuse users who type `ctrl++` (intending Ctrl+Plus) since `split('+')` would produce `["ctrl", "", ""]` and the empty-string arm would trigger `ChordParseError::Empty`. The `+` key is effectively unbindable through the TOML format.

**Severity**: T4 (edge case, `+` key unbindable)

### 3.7 Schema Diff Command Integration

**File**: `schema_diff.rs`

- This module provides the older single-table `:diff-schema` command, which is distinct from the newer `narwhal-schema-diff` crate's cross-connection `:schema-diff` command.
- MySQL MODIFY COLUMN coalescing is correctly implemented here too (same pattern as the schema-diff MySQL emitter).
- The module correctly warns about the trust boundary on `Column::default` — no escaping, no sandboxing, user must review.

**Finding T3-21** — `schema_diff.rs:62-68`: `columns_equivalent()` compares `data_type` and `default` as raw strings without normalisation. This means `varchar(255)` vs `character varying(255)` would show as a type change here, even though the `narwhal-schema-diff` crate's `canonical_type()` would treat them as equivalent. The two diff implementations are inconsistent in their type comparison.

**Severity**: T3 (inconsistency between `schema_diff.rs` and `narwhal-schema-diff` crate in type comparison, phantom diffs from the older command)

---

## Summary Table

| ID | Severity | Module | File:Line | Description |
|----|----------|--------|-----------|-------------|
| T3-1 | T4 | narwhal-sql | `treesitter/parser.rs:62-63` | Redundant `ok_or` after `Some` insertion |
| T3-2 | T4 | narwhal-sql | `treesitter/edit.rs:99-109` | Linear `byte_position()` scan, no doc |
| T3-3 | T3 | narwhal-sql | `treesitter/scope.rs:46-51` | Zero-width `descendant_for_byte_range` may pick wrong node at exact boundaries |
| T3-4 | T4 | narwhal-sql | `treesitter/highlight.rs:166-172` | `identifier_kind` may miss `TableRef` for bare identifiers under `from` |
| T3-5 | T4 | narwhal-sql | `lint.rs:89-92` | Multi-line `SELECT *` (SELECT and * on different lines) not caught |
| T3-6 | T4 | narwhal-sql | `lint.rs:195-200` | `DELETE FROM` arm redundant with `DELETE ` arm |
| T3-7 | N/A | narwhal-sql | `lint.rs:272-280` | Correct: HAVING doesn't silence cartesian join |
| T3-8 | T4 | narwhal-sql | `guard.rs:53-56` | `WITH ... INSERT` classified as Read (documented, handled deeper) |
| T3-9 | T4 | narwhal-sql | `guard.rs:97-98` | `SET` over-classified as Ddl (conservative, acceptable) |
| T3-10 | T3 | narwhal-sql | `splitter.rs:210-215` | Backtick state doesn't handle escaped (doubled) backticks |
| T2-1 | T3 | schema-diff | `diff.rs:70-78` | Missing system schemas (`pg_temp`, `INFORMATION_SCHEMA`, `backup`) |
| T2-2 | T3 | schema-diff | `diff.rs:126-131` | Case-insensitive column matching collapses distinct PG columns |
| T2-3 | T4 | schema-diff | `diff.rs:170-171` | Index comparison limited by `Index` struct (no partial/ops class) |
| T2-4 | T3 | schema-diff | `normalise.rs:35-45` | Missing `int` → `int4` mapping causes phantom type diffs |
| T2-5 | T3 | schema-diff | `normalise.rs:41` | Precision-qualified timestamp synonyms not handled |
| T2-6 | T3 | schema-diff | `normalise.rs:82-92` | PostgreSQL `::type` cast suffix in defaults not stripped |
| T4-1 | T4 | schema-diff | `emit/generic.rs:139-141` | Double space when table arg is empty in CREATE INDEX |
| T2-7 | T4 | schema-diff | `emit/postgres.rs:159-161` | `rsplit('.')` breaks on dot-containing table names |
| T2-8 | T3 | schema-diff | `emit/mysql.rs:138-142` | `/* keep existing type */` inline comment produces invalid SQL |
| T2-9 | T4 | schema-diff | `emit/sqlite.rs:133-137` | Debug formatting (`{:?}`) of `Option<String>` in user-facing comment |
| T2-10 | T3 | schema-diff | `emit/mssql.rs:130-135` | Synthesised default constraint name won't match auto-named constraints |
| T2-11 | T3 | schema-diff | `emit/mssql.rs:188-194` | Double-constraint risk if drop fails on wrong name |
| T3-11 | T4 | commands | `commands.rs:557-558` | Pop-based extraction reads backwards |
| T3-12 | T4 | commands | `commands.rs:348-355` | Format stored as String, not enum (design choice) |
| T3-13 | T4 | commands | `commands.rs:417-422` | Chart `-t` flag inconsistent with diagram `-t` |
| T3-14 | T4 | commands | `export/csv.rs` | No UTF-8 BOM (design choice, interoperability concern) |
| T2-12 | T2 | commands | `export/parquet.rs:47-50` | Silent data loss when row 101+ introduces unseen type |
| T2-13 | T4 | commands | `export/parquet.rs:178-179` | Timestamp+Int64 → Utf8 widening (documented design) |
| T3-15 | N/A | commands | `export/parquet.rs:222-225` | Bool→Int64, Bool→Float64 coercion correct |
| T3-16 | T4 | commands | `export/parquet.rs:82-84` | Staging path collision (theoretical, single-threaded app) |
| T3-17 | T4 | commands | `cell_edit.rs:31-38` | No undo stack in module (host responsibility) |
| T3-18 | T4 | commands | `cell_edit.rs:111-112` | `.contains("BOOL")` over-broad matching |
| T3-19 | T4 | commands | `keymap.rs:88-89` | Missing `"spc"` alias for space key |
| T3-20 | T4 | commands | `keymap.rs:97-99` | `+` key unbindable through TOML format |
| T3-21 | T3 | commands | `schema_diff.rs:62-68` | Type comparison not normalised, inconsistent with schema-diff crate |

---

## Actionable Recommendations (T2–T3 only)

1. **T2-12 (Parquet silent data loss)**: Add a `tracing::warn!` when `append_value()` hits the `(typed, _) => typed.append_null()` fallback for a non-null value. This alerts the user that data was lost due to type inference mismatch.

2. **T2-4 (Missing `int` → `int4` mapping)**: Add `("int", "int4")` to the `SYNONYMS` table in `normalise.rs`. Ensure it comes after `("integer", "int4")` to avoid matching the prefix.

3. **T2-5 (Precision-qualified timestamps)**: Strip the precision qualifier `(n)` from timestamp types before synonym matching, or add explicit mappings for `timestamp(0) without time zone` etc.

4. **T2-6 (`::type` cast suffix)**: In `canonical_default()`, strip trailing `::identifier` sequences after lowercasing. E.g., `'foo'::text` → `'foo'`.

5. **T2-8 (MySQL inline comment)**: Move `/* keep existing type */` from inside the MODIFY COLUMN statement to a comment line above it.

6. **T2-1 (Missing system schemas)**: Add `"pg_temp"`, `"INFORMATION_SCHEMA"` (uppercase), and `"backup"` to `is_system_schema()`.

7. **T3-10 (Backtick escaping)**: Add doubled-backtick handling to the `Backtick` state in the splitter, similar to how `QuotedIdentifier` handles `""`.

8. **T3-21 (Type comparison inconsistency)**: Apply `canonical_type()` in `schema_diff.rs:columns_equivalent()` or delegate to the schema-diff crate for single-table diffs.

9. **T3-3 (Zero-width scope detection)**: Use `descendant_for_byte_range(offset, offset.saturating_add(1))` with a 1-byte range to prefer the node *containing* the offset over the node *starting at* it.
