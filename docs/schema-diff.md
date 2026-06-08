# Schema diff

Compute the structural difference between two database schemas and
emit dialect-aware migration DDL.

## TUI

```
:schema-diff <source-conn> <target-conn>
```

Opens a side-by-side modal: tables and columns present only in the
source, only in the target, and shape changes (type / nullability /
default / FK / index). Press `y` to yank the generated DDL to the
clipboard, `:w <path>` to save it.

## Headless

```sh
narwhal schema-diff src=prod tgt=staging > migration.sql
```

Useful in CI: catch drift between a production database and a
staging replica before the next deploy.

Flags:

- `--dialect <postgres|mysql|sqlite|mssql>` — override target
  dialect detection
- `--no-data` — emit DDL only (the default; data diff is not
  supported yet)
- `--include-schemas a,b` — restrict to a comma-separated set

## What's compared

| Element                | Compared | Notes                              |
|------------------------|----------|------------------------------------|
| Tables                 | ✅       |                                    |
| Columns (type, null, default) | ✅ | Type aliases normalised (`int4` = `int`) |
| Primary keys           | ✅       |                                    |
| Foreign keys           | ✅       | Cross-schema FKs dropped in v1     |
| Unique constraints     | ✅       |                                    |
| Check constraints      | ✅       | String-equality only               |
| Indexes (excl. PK / UK)| ✅       |                                    |
| Views                  | ⏳       | Reserved for a future release      |
| Stored procedures      | ⏳       | Reserved for a future release      |
| Triggers               | ⏳       | Reserved for a future release      |

## Dialect emitters

| Dialect    | Emitter status | Notes                                  |
|------------|----------------|----------------------------------------|
| PostgreSQL | ✅             | `IF NOT EXISTS` guards on `ADD COLUMN` |
| MySQL      | ✅             | Per-column `MODIFY COLUMN` statements  |
| SQLite     | ✅             | Limited — many alters require table rebuild |
| MSSQL      | ✅             | Schema-qualified identifiers           |
| Generic    | ✅             | ANSI-ish; fallback when no engine match|

A diff between two unrelated dialects falls back to the generic
emitter and prints a warning.
