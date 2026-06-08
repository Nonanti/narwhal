# Headless `exec`

Run a SQL file or one-liner without launching the TUI. Useful for
cron, CI, shell pipelines, and ad-hoc scripts.

## Usage

```sh
narwhal exec --conn prod < migration.sql
narwhal exec --conn prod --sql 'SELECT 1'
narwhal exec --conn prod --file migration.sql
```

## Output formats

```sh
narwhal exec --conn prod --sql 'SELECT * FROM orders LIMIT 5' --format csv
narwhal exec --conn prod --sql 'SELECT * FROM orders LIMIT 5' --format json
narwhal exec --conn prod --sql 'SELECT * FROM orders LIMIT 5' --format markdown
narwhal exec --conn prod --sql 'SELECT * FROM orders LIMIT 5' --format table
```

| Format     | Notes                                            |
|------------|--------------------------------------------------|
| `csv`      | RFC 4180; quoting handles embedded commas/newlines |
| `tsv`      | Tab-separated, same quoting                      |
| `json`     | One array of objects                             |
| `jsonl`    | One object per line                              |
| `markdown` | Pipe table; alignment from column type           |
| `table`    | ASCII table, fixed-width                         |
| `insert`   | Generated `INSERT INTO ... VALUES (...)` rows    |
| `parquet`  | Columnar; needs `--out <path>`                   |

## Safety

Mutations are refused unless `--write` is passed. `--read-only`
short-circuits regardless of `--write` for connections you do not
trust:

```sh
narwhal exec --conn prod --sql 'UPDATE orders SET ...'
# Error: SQL contains a mutation; pass --write to allow.

narwhal exec --conn prod --read-only --sql 'UPDATE orders SET ...' --write
# Error: --read-only blocks every mutation.
```

## Exit codes

| Code | Meaning                            |
|------|------------------------------------|
| 0    | Success                            |
| 1    | Connection / authentication error  |
| 2    | SQL parse / runtime error          |
| 3    | Mutation refused without `--write` |
| 4    | I/O error reading the SQL source   |

## Examples

### Cron — nightly export

```sh
0 2 * * * narwhal exec --conn warehouse --file /opt/etl/orders.sql \
  --format parquet --out /backup/orders-$(date +\%F).parquet
```

### CI — drift check

```sh
narwhal schema-diff src=staging tgt=prod | tee migration.sql
test ! -s migration.sql || (echo "Drift detected"; exit 1)
```

### Shell — quick row count

```sh
narwhal exec --conn prod --sql 'SELECT count(*) FROM orders' --format jsonl \
  | jq -r '.["count"]'
```
