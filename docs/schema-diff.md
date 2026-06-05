# Schema diff (v2.0+)

`narwhal schema-diff <source> <target>` introspects two connections,
computes a structural diff, and emits DDL that migrates `target`
onto `source`. The same logic powers the in-TUI `:schema-diff`
command (Phase B; see [Implementation status](#implementation-status)).

The output is **deterministic** — the same pair of schemas always
produces byte-identical SQL, so a CI job can hash it and fail on
drift.

## TL;DR

```bash
# Show the migration that brings staging up to prod
narwhal schema-diff prod staging

# Write to a file for review
narwhal schema-diff prod staging --out prod-to-staging.sql

# Pick a dialect explicitly (defaults to the source driver)
narwhal schema-diff prod staging --dialect postgres

# CI gate: non-zero exit when there's any drift
narwhal schema-diff prod staging --fail-on-drift

# Narrow to one schema or one table
narwhal schema-diff prod staging --schema public --table users

# Map schemas between environments
narwhal schema-diff prod staging --schema-map prod_app=staging_app
```

## Direction

The naming is deliberate: **source = desired**, **target = current**.
The emitted DDL transforms `target` into `source`. So a table that
exists in `source` but not `target` is **added** (we will run
`CREATE TABLE` against `target`).

If you read "diff" as "git diff" — `source` is the "after" side and
`target` is the "before". Running the SQL applies the "after" state.

## What is compared

| Object | Diff covers |
|---|---|
| **Tables** | added / removed / changed (any column / index / FK / unique delta) |
| **Columns** | added / removed / type / nullable / default |
| **Indexes** | added / removed / shape (columns, uniqueness) |
| **Foreign keys** | added / removed / shape (columns, referenced table, `ON UPDATE` / `ON DELETE`) |
| **Unique constraints** | added / removed / column set |

Out of scope for v2.0 (will land in v2.4+):

- views, materialised views, functions, procedures
- sequences, custom types, extensions
- permissions / GRANTs
- row-level data diffs

System schemas (`pg_catalog`, `information_schema`, `pg_toast`,
`sys`, `mysql`, `performance_schema`, `sys_schema`) are filtered out
unconditionally — exact-match so a user schema named
`pg_catalog_clone` survives.

## Type & default normalisation

Two strings that mean the same thing are treated as equal:

| Source side | Target side | Diff? |
|---|---|---|
| `character varying(255)` | `varchar(255)` | no |
| `integer` | `int4` | no |
| `double precision` | `float8` | no |
| `timestamp with time zone` | `timestamptz` | no |
| `NULL` | (no default) | no |
| `(0)` | `0` | no |
| `current_timestamp` | `CURRENT_TIMESTAMP` | no |

Anything else is reported verbatim — driver quirks (length
modifiers, character sets, collations) that *might* be semantically
equivalent are flagged because the right answer depends on context.

## Dialects

`--dialect` accepts `postgres` (also `postgresql`, `pg`), `mysql`
(also `mariadb`), `sqlite`, `mssql` (also `sqlserver`), and
`generic` (the ANSI fallback). When omitted, the source connection's
driver name is used — so `narwhal schema-diff prod staging` against
two Postgres connections automatically picks the `postgres` emitter.

| Capability | postgres | mysql | sqlite | mssql | generic |
|---|---|---|---|---|---|
| `CREATE TABLE` / `DROP TABLE` | ✅ | ✅ | ✅ | ✅ | ✅ |
| `ADD COLUMN` / `DROP COLUMN` | ✅ | ✅ | ✅¹ | ✅ | ✅ |
| `ALTER COLUMN TYPE` | ✅ (`USING`) | ✅ (`MODIFY`) | ⚠️² | ✅ | ⚠️ comment |
| `SET / DROP NOT NULL` | ✅ | ✅ (`MODIFY`) | ⚠️² | ✅ | ✅ |
| `SET / DROP DEFAULT` | ✅ | ✅ (`MODIFY`) | ⚠️² | ✅ (named constraint) | ✅ |
| `CREATE / DROP INDEX` | ✅ | ✅ (`ON <table>`) | ✅ | ✅ (`ON <table>`) | ✅ |
| `ADD / DROP FK` | ✅ | ✅ (`DROP FK`) | ⚠️² | ✅ | ✅ |
| `ADD / DROP UNIQUE` | ✅ | ✅ | ⚠️² | ✅ | ✅ |

¹ `DROP COLUMN` requires SQLite >= 3.35. The emitter adds an inline
comment.
² SQLite has no `ALTER COLUMN TYPE` / `NULL` / `DEFAULT` at all, and
constraints live inside the table body. Affected changes are
emitted as `-- table rebuild needed: …` comments; the operator
performs the rebuild idiom (create-new → insert select → drop-old →
rename) by hand.

### Drop-before-recreate ordering

When a constraint shape changes, the emitter drops the old form
before applying column work, then recreates it with the new shape.
For postgres / mysql / mssql the order is:

1. Drop changing FKs, then UNIQUE, then indexes
2. (MSSQL) Drop default constraints for columns whose default
   changed
3. Column changes (add / drop / type / null / default)
4. (MSSQL) Re-add default constraints
5. Recreate indexes, then UNIQUE, then FKs

So a `users.email` type change with a referencing `fk_orders_user`
won't trip the engine's "cannot alter under constraint" check —
provided the FK is itself in the diff.

### Known limitation: unchanged FKs that touch changed columns

If `users.id` changes type but the diff doesn't include any change
to `fk_orders_user` (because `orders` is otherwise identical),
the emitter will *not* drop / recreate that FK. The engine will
refuse the `ALTER COLUMN` and surface its own error. A future
enhancement extends the diff context to detect cross-table FK
reach; in the meantime, hand-edit the emitted DDL or use
`--schema-map` to narrow the diff to a clean unit.

## Flags

| Flag | Purpose |
|---|---|
| `--dialect <name>` | Override the auto-picked dialect |
| `-o`, `--out <path>` | Write DDL to file (default: stdout) |
| `--schema <name>` | Restrict diff to one schema (both sides) |
| `--table <name>` | Restrict diff to one table |
| `--schema-map src=tgt` | Rename target-side schemas before diffing. Repeatable. |
| `--fail-on-drift` | Exit code 2 when the diff is non-empty (CI gate) |

`--schema-map` is the answer for environments that use different
namespaces. `--schema-map prod_app=staging_app` rewrites every
target table whose schema is `staging_app` to be `prod_app` before
the diff runs, so a perfectly-aligned schema under different names
no longer registers as drift.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Diff completed (regardless of whether changes were emitted) unless `--fail-on-drift` is set |
| `1` | Configuration / connection / introspection error |
| `2` | `--fail-on-drift` set and at least one change was emitted |

## Examples

### Apply directly to the target

```bash
narwhal schema-diff prod staging --dialect postgres | psql "$STAGING_URL"
```

Because the output is deterministic and the dialect emitters honour
proper drop-before-recreate ordering, the piped form is a viable
mechanism for "one-shot make staging look like prod". Operators in
compliance-sensitive environments should still write to a file,
review, and apply with their preferred runner — `schema-diff` is a
generator, not a migration tool.

### CI drift gate

```yaml
# .github/workflows/schema-drift.yml
- name: detect schema drift
  run: |
    narwhal schema-diff prod staging --fail-on-drift > /tmp/drift.sql
  continue-on-error: false

- name: upload drift artefact
  if: failure()
  uses: actions/upload-artifact@v4
  with:
    name: schema-drift.sql
    path: /tmp/drift.sql
```

The job fails as soon as `prod` and `staging` diverge; the artefact
captures the would-be migration so reviewers see exactly what
changed.

### Narrow scope

```bash
# Just the `analytics` schema
narwhal schema-diff prod staging --schema analytics

# Just one table
narwhal schema-diff prod staging --schema public --table accounts
```

### Cross-environment renames

```bash
narwhal schema-diff prod staging \
  --schema-map prod_app=staging_app \
  --schema-map prod_log=staging_log
```

## Implementation status

| Component | Status |
|---|---|
| `narwhal-schema-diff` crate (diff + emitters) | ✅ shipped in v2.0 |
| `narwhal schema-diff` CLI subcommand | ✅ shipped in v2.0 |
| `:schema-diff` TUI command (dump to new editor tab) | 🔄 follow-up (step 3 phase B) |
| Full TUI modal (tree + DDL preview, yank/save bindings) | 🔮 v2.1 polish |
| Postgres testcontainers round-trip test | 🔄 follow-up |
| Cross-table FK reach analysis | 🔮 v2.4 |
| View / function / procedure diff | 🔮 v2.4 |

## See also

- [`audit.md`](audit.md) — audit log captures every `:schema-diff`
  execution as a `Configuration` event.
- [`narwhal-schema-diff/src/lib.rs`](../crates/narwhal-schema-diff/src/lib.rs)
  — crate-level documentation including the `SchemaDiff` wire format.
- [Atlas](https://atlasgo.io/) — prior art for declarative schema
  migration. narwhal's scope is narrower (no two-way diff, no
  migration runner) but the diff direction convention is the same.
