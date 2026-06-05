# Microsoft SQL Server driver

Narwhal speaks TDS through [tiberius] — a pure-Rust SQL Server
client. The driver lands behind the `mssql` cargo feature inside the
`narwhal-drivers` umbrella; the default `narwhaldb` binary turns it on
via the `all-drivers` feature.

```toml
# Cargo.toml — library consumers only
narwhal-drivers = { version = "1", default-features = false, features = ["mssql"] }
```

[tiberius]: https://github.com/prisma/tiberius

## Connection URL

```text
mssql://user:password@host:1433/database?key=value&key=value
sqlserver://...   # alias, matches the JDBC convention
```

Both schemes parse identically. The query string accepts the same TLS
knobs as PostgreSQL (`sslmode=`, `sslrootcert=`) plus MSSQL-specific
keys documented under [Options](#options) below.

## Authentication

| Mode | Status | Notes |
| ---- | :----: | ----- |
| SQL Server Authentication (user + password) | **supported** | Default. Username goes in the URL or `username` field; password resolves through the keychain / `${preconnect:…}` chain like every other driver. |
| Windows / SSPI Integrated | **deferred** (v2.4) | Requires building tiberius with the `winauth` feature; the published binary does not. Setting `integrated_security=true` returns a clear `Error::Config` at connect time so you know to wait or use SQL Auth. |
| Kerberos / GSSAPI on Linux | **deferred** (v2.4) | Requires tiberius `integrated-auth-gssapi` plus a working `kinit`. Same explicit error path. |
| Azure AD token | not exposed | Tiberius supports it; surfacing through the config layer is a follow-up. |

## TLS

Narwhal's `ssl_mode` maps to tiberius' `EncryptionLevel`:

| `ssl_mode`     | `EncryptionLevel` | Hostname verification |
| -------------- | ----------------- | --------------------- |
| `disable`      | `NotSupported` (`DANGER_PLAINTEXT`) | — |
| `prefer` (default) | `On` | system trust store |
| `require`      | `On` | system trust store |
| `verify-ca`    | `Required` | system trust store |
| `verify-full`  | `Required` | system trust store |

Tiberius is built with rustls — no `openssl-sys`. The system trust
store + an optional `ssl_root_cert` provide the trust anchors.
`trust_server_certificate=true` skips certificate validation entirely;
it is the only way to talk to the official
`mcr.microsoft.com/mssql/server` image without a CA-signed cert, but
should **never** be used against production.

The `encrypt` query-string key (ADO.NET-style) overrides `ssl_mode`
when both are set:

```text
mssql://sa:pw@host:1433/master?encrypt=true&trust_server_certificate=true
```

## Options

| Key                        | Allowed values         | Default | Description |
| -------------------------- | ---------------------- | :-----: | ----------- |
| `application_name`         | any string             | unset   | Sets `APP_NAME()` on the server. Shows up in `sys.dm_exec_sessions`. |
| `trust_server_certificate` | `true`/`false`/`yes`/`no` | `false` | Skip certificate validation. Dev/test only. Case-insensitive. |
| `instance_name`            | non-empty string       | unset   | Named instance. Also set `port=` to the dynamically-assigned port (we don't query SQL Browser). |
| `encrypt`                  | `true`/`false`/`DANGER_PLAINTEXT` | derived from `ssl_mode` | Overrides `ssl_mode`. Matches ADO.NET semantics. |
| `connect_timeout`          | positive integer (seconds) | `10`    | Whole-handshake budget (TCP + TLS + LOGIN7). Defaults to 10 s so a black-holed host fails fast instead of hanging the TUI for ~2 minutes (Linux SYN retransmit default). |
| `integrated_security`      | `true`/`false`         | `false` | Rejected today (see [Authentication](#authentication)). Case-insensitive. |

Any other key is rejected at connect time with `Error::Config` to
prevent silent misconfiguration.

## Named instances

Narwhal does **not** ship the SQL Browser discovery feature — the
binary stays runtime-agnostic and pulling in tiberius'
`sql-browser-tokio` adds a UDP path that doesn't fit the rest of the
stack. To connect to a named instance:

```text
mssql://sa:pw@host:14333/db?instance_name=SQLEXPRESS
```

Resolve the dynamic port out-of-band (`sqlcmd -L`, the SQL Server
Configuration Manager, etc.) and feed it via the `port=` URL segment.
The `instance_name` is forwarded to tiberius so the LOGIN7 packet
correctly addresses the instance.

## Streaming

Tiberius' `QueryStream` borrows the client for its lifetime, which
conflicts with the per-connection async mutex narwhal-pool wraps
around every driver. The MSSQL driver therefore **materialises** the
result inside `execute`/`stream` and replays through a buffered row
stream. `Capabilities::streaming = false` so the TUI's progress UI
shows a "loading…" placeholder instead of an incremental cursor.

Server-side cursoring (FAST_FORWARD) is a v2.x follow-up.

## Cancellation

Tiberius has no out-of-band cancellation message, so the driver
reports `cancel_handle() = None` and `Capabilities::cancellation =
false`. Pressing the cancel hotkey aborts the await on the client
side, drops the connection, and surfaces `Error::Cancelled` to the
caller; the server may still finish the in-flight query before
noticing the closed socket.

A future minor release may add a `KILL <SPID>` shim opened on a
second connection (matching the MySQL driver's pattern). It will be
gated behind an opt-in option key because killing sessions is more
destructive than the postgres CancelRequest path.

## Known limitations

- `release_savepoint` is a no-op. SQL Server's `SAVE TRAN` has no
  release counterpart; savepoints disappear at `COMMIT`/`ROLLBACK`.
  Generic transaction wrappers can keep calling `release_savepoint`
  without branching on driver name.
- `set_read_only(true)` is enforced **at the driver layer**, not by
  the server. The driver stores the flag and refuses any subsequent
  mutating statement with `Error::Unsupported` before it reaches the
  wire. This satisfies the trait contract ("engine refuses writes
  for the lifetime of the session") without depending on a per-
  database SQL Server flag. Defaults to `false`; honours
  `[connections.<name>.read_only = true]` from the config so a
  production connection is read-only from the first statement.
- Spatial types (`geometry`, `geography`) come back as
  `Value::Unknown` — the wire format is well-defined but inflating
  them into something more useful is a v2.x type-system task.
- SQL_VARIANT cells return their textual rendering when one is
  available, `Value::Unknown(<sql_variant>)` otherwise.

## Testing

```sh
# Pure-Rust unit + binding tests, no network.
cargo test -p narwhal-drivers --features mssql --test mssql_binding

# Integration tests against an ephemeral container (Docker required).
cargo test -p narwhal-drivers --features mssql --test mssql_smoke -- --ignored

# Long-lived dev container via docker-compose.
docker compose -f crates/narwhal-drivers/tests/fixtures/mssql/docker-compose.yml up -d
NARWHAL_MSSQL_HOST=127.0.0.1 \
NARWHAL_MSSQL_USER=sa \
NARWHAL_MSSQL_PASSWORD='yourStrong(!)Password' \
cargo test -p narwhal-drivers --features mssql --test mssql_smoke external_select_one -- --ignored
```

See [`tests/fixtures/mssql/README.md`](../../crates/narwhal-drivers/tests/fixtures/mssql/README.md)
for the full fixture workflow.
