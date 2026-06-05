# MSSQL fixture

The integration suite under `tests/mssql_smoke.rs` exercises the driver
two ways:

| Mode | Trigger | What you need |
| ---- | ------- | ------------- |
| `testcontainers` | `cargo test -p narwhal-drivers --features mssql -- --ignored` | Docker daemon |
| Long-lived dev container | `NARWHAL_MSSQL_HOST=… cargo test … external_select_one -- --ignored` | a server you manage (docker-compose, Azure SQL DB, …) |

The `testcontainers` mode pulls
`mcr.microsoft.com/mssql/server:2022-latest`, waits for the SA login to
become available, then runs the full test set against an ephemeral
container. The container is reaped at the end of every test.

For interactive iteration the docker-compose file in this directory
brings up a single long-lived container so the image isn't re-pulled
between cargo runs:

```sh
docker compose -f crates/narwhal-drivers/tests/fixtures/mssql/docker-compose.yml up -d

NARWHAL_MSSQL_HOST=127.0.0.1 \
NARWHAL_MSSQL_USER=sa \
NARWHAL_MSSQL_PASSWORD='yourStrong(!)Password' \
cargo test -p narwhal-drivers --features mssql --test mssql_smoke -- --ignored
```

When you're done:

```sh
docker compose -f crates/narwhal-drivers/tests/fixtures/mssql/docker-compose.yml down -v
```

## CI notes

The `#[ignore]` gate means default `cargo test` runs never need Docker.
Wire the integration tests into CI by setting `NARWHAL_MSSQL_HOST` (or
using the testcontainers mode if the runner has the Docker daemon
mounted) and passing `-- --ignored` to `cargo test`. The unit and
binding tests under `mssql_binding.rs` run on every `cargo test` and do
not need a database — keep them green as the baseline.
