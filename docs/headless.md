# Headless `narwhal exec`

Run any query without the TUI — for cron, CI, shell pipelines, and
ad-hoc one-liners.

```sh
narwhal exec --conn prod 'SELECT count(*) FROM users'
narwhal exec -c prod -f csv 'SELECT * FROM orders' > orders.csv
narwhal exec -c prod -f json 'SELECT id, email FROM users' | jq '.[].email'
```

## Formats

`-f` / `--format` accepts: `table` (default), `csv`, `json`, `tsv`,
`markdown`, `parquet`. Parquet honours the `--compression` flag
(`snappy`, `zstd`, `none`).

## Write safety

Writes are wrapped in a `BEGIN … ROLLBACK` envelope by default — you
see the rows that *would* change without committing. Opt out with
`--write`:

```sh
narwhal exec -c prod --write 'UPDATE users SET banned = true WHERE id = 42'
```

The `--write` flag also bypasses the connection's `read_only` /
`confirm_writes` guards, so reserve it for trusted scripts.

## Exit codes

| Code | Meaning |
|---|---|
| 0   | success |
| 1   | SQL error |
| 2   | connection error |
| 3   | configuration error |
| 4   | write attempted without `--write` |

Useful in CI gates and `Makefile` recipes.
