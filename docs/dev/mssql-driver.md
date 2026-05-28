# MSSQL driver (tiberius)

Design notes for the Microsoft SQL Server backend (added in v2.0).
The schema diff and audit subsystems consume the connection and
schema introspection shapes documented in
[§ Connection introspection format](#connection-introspection-format).

## What landed

Microsoft SQL Server is now a first-class engine alongside
PostgreSQL, MySQL, SQLite, DuckDB and ClickHouse. The driver follows
the post- layout — a single `mssql` cargo feature inside the
`narwhal-drivers` umbrella — and the post- trait convention
(native `async fn` in trait, `Box<dyn DynConnection>` at trait-object
sites, no `#[async_trait]`).

| Item  | Where  |
| ------------------------------------------ | ------------------------------------------------------ |
| Feature flag  | `narwhal-drivers/Cargo.toml` → `mssql`  |
| Driver module  | `crates/narwhal-drivers/src/mssql/{mod,ddl,types}.rs`  |
| Registry entry  | `narwhal-drivers/src/registry.rs` (cfg-gated)  |
| URL scheme  | `narwhal-config/src/url.rs` (`mssql://`, `sqlserver://`) |
| Workspace deps  | `tiberius` 0.12, `tokio-util` 0.7 (compat)  |
| Unit + binding tests  | `tests/mssql_binding.rs`  |
| Integration tests (`#[ignore]`)  | `tests/mssql_smoke.rs`  |
| Docker-compose fixture  | `tests/fixtures/mssql/docker-compose.yml`  |
| User docs  | `docs/drivers/mssql.md`  |

The umbrella `all-drivers` feature pulls `mssql` in, so the
`narwhaldb` binary keeps shipping every backend out of the box.

## Convention notes for downstream readers

### `Connection` trait, RPITIT-style

The driver implements `narwhal_core::Connection` directly with
`async fn` bodies. `connect` returns `Box<dyn DynConnection>` — the
dyn-safe sibling — so the pool, app and MCP layers continue to talk
to MSSQL through the same trait object they use for the other
engines. See [`docs/dev/async-trait-style.md`](async-trait-style.md)
for the full rationale.

### `SchemaCatalog` shape

`list_all_tables` returns the workspace's canonical
`SchemaCatalog = Vec<(Schema, Vec<Table>)>` alias. Order is
deterministic: schema order from `list_schemas`, table order from
`sys.tables` joined with `sys.schemas` (ASCII collation). schema diff and audit tools that snapshot the catalogue can rely on
this stability.

### Cargo-feature gating

```toml
mssql = [
  "dep:tiberius",
  "dep:tokio-util",
  "dep:futures",
  "dep:chrono",
  "dep:uuid",
]
all-drivers = ["postgres", "mysql", "sqlite", "duckdb", "clickhouse", "mssql"]
```

`tiberius` is pulled with `default-features = false, features =
["chrono", "rustls", "tds73"]`. That keeps the workspace clear of
`openssl-sys` (rustls only) and locks in TDS 7.3 (SQL Server 2008+)
so we can rely on the `date` / `time` / `datetime2` / `datetimeoffset`
wire types — every supported edition (Express → Enterprise → Azure
SQL DB) speaks TDS 7.3 or newer.

## Connection introspection format

> and snapshot the schema/connection shape produced by
> this driver. The serialisations below are what those tools should
> expect to see on disk.

### `ConnectionParams` for MSSQL

```toml
host  = "sql.prod.example.com"
port  = 1433  # default; can be omitted
database  = "appdb"  # SQL Server "master" default if omitted
username  = "appuser"
ssl_mode  = "verify-full"  # → EncryptionLevel::Required

# Optional, MSSQL-specific:
[connections.appdb.options]
application_name  = "narwhal-tui"  # APP_NAME on the server
trust_server_certificate = "true"  # dev/test only
encrypt  = "true"  # overrides ssl_mode
instance_name  = "SQLEXPRESS"  # SQL Browser instance
integrated_security  = "false"  # see note
```

Recognised option keys (the whitelist enforced at connect time):

| Key  | Maps to  |
| -------------------------- | ---------------------------------------- |
| `application_name`  | `Config::application_name`  |
| `trust_server_certificate` | `Config::trust_cert`  |
| `instance_name`  | `Config::instance_name` (port required)  |
| `encrypt`  | `Config::encryption(…)` override  |
| `integrated_security`  | Reserved; currently rejected, see below  |

Anything else is rejected with `Error::Config("unsupported connection
option: …")` — the same hardening pattern the PostgreSQL driver uses,
to prevent typo-induced silent defaults and to keep the audit log
deterministic.

### `Schema` / `Table` payloads

`describe_table` returns the engine-agnostic
`narwhal_core::TableSchema` already used by PG / MySQL / DuckDB. The
MSSQL-specific knobs surface like this:

- `Column::data_type`: lower-cased rendering of the
  `INFORMATION_SCHEMA.COLUMNS.DATA_TYPE`, augmented with the length /
  precision tuple where applicable. Examples: `int`, `nvarchar(255)`,
  `nvarchar(max)`, `decimal(18,4)`, `datetime2`, `uniqueidentifier`.
- `Column::default`: copied verbatim from
  `sys.default_constraints.definition`, which is already parenthesised
  (`((0))`, `(getdate)`, `(N'literal')`). The DDL emitter replays it
  as-is so the round-trip is byte-stable.
- `Index::primary` mirrors `sys.indexes.is_primary_key`.
- `ForeignKey::on_update` / `on_delete` map the
  `update_referential_action_desc` / `delete_referential_action_desc`
  text (`NO_ACTION`, `CASCADE`, `SET_NULL`, `SET_DEFAULT`). SQL Server
  does not expose RESTRICT separately; it is treated as `NoAction`.
- `UniqueConstraint`s are derived from `sys.indexes` (every
  `is_unique = 1 AND is_primary_key = 0` row) so the primary-key index
  is not double-reported via the unique list.

### `fetch_ddl` output

```sql
CREATE TABLE [dbo].[customers] (
  [id] int IDENTITY(1,1) PRIMARY KEY,
  [email] nvarchar(255) NOT NULL,
  [created_at] datetime2 DEFAULT (sysutcdatetime) NOT NULL
);
```

- Identifiers are quoted with brackets, doubling any embedded `]`
  (matches `QUOTENAME`).
- IDENTITY seed/increment are read from `sys.identity_columns` and
  replayed; missing values default to `(1,1)`.
- Composite primary keys are emitted as a trailing
  `PRIMARY KEY (col1, col2)` clause instead of inline annotations.
- FKs and indexes are **not** emitted in v1 of the DDL builder; they
  surface separately through `describe_table` and the schema-diff tool
  can stitch them. Same trade-off the PostgreSQL DDL builder makes.

## Tricky bits worth re-reading

- **TLS encryption knobs.** `ssl_mode` drives the default
  (`Prefer`/`Require` → `On`, `VerifyCa`/`VerifyFull` → `Required`,
  `Disable` → `NotSupported` / `DANGER_PLAINTEXT`). The `encrypt`
  option key, when present, wins — it lets users round-trip ADO.NET-
  style connection strings without us having to map every casing.
  `trust_server_certificate` is the only way to talk to the official
  Docker image without setting up a CA.
- **No out-of-band cancellation.** Tiberius offers no equivalent of
  the PostgreSQL CancelRequest message. `cancel_handle` returns
  `None`, `Capabilities::cancellation` is `false`. A `KILL SESSION`
  shim was considered and rejected — sessions outlive individual
  queries, so we would have killed unrelated traffic. Future v2.x
  work: open a second connection and issue `KILL <SPID>` (mysql
  driver pattern), gated behind a feature flag.
- **`release_savepoint` is a no-op.** SQL Server's `SAVE TRAN`
  pairs only with `ROLLBACK TRAN <name>`; there is no
  `RELEASE SAVEPOINT` counterpart. Reporting the call as a no-op
  keeps generic transaction wrappers (TUI, MCP) ergonomic — they can
  always call release without branching on the driver name.
- **`set_read_only(true)` enforces at the driver layer.** SQL Server
  has no per-session READ-ONLY flag (the database-level equivalent
  is global and disruptive). The driver stores the flag in an
  `Arc<AtomicBool>` and refuses any mutating statement in
  `Connection::execute` with `Error::Unsupported` before it reaches
  the wire — satisfying the trait's "engine refuses writes for the
  lifetime of the session" contract without depending on server-side
  enforcement. The TUI's syntactic guard
  (`narwhal_sql::guard_read_only`) is the earlier, syntactic layer.
  Snapshot isolation is **not** used as a substitute because
  snapshot iso does not, by itself, prevent writes. See the
  [Read-only enforcement](#read-only-enforcement) section for the
  full design.
- **GUID byte order.** SQL Server stores `uniqueidentifier` with the
  classic mixed-endian layout (first 8 bytes byte-swapped vs. RFC
  4122). Tiberius' `ColumnData::Guid` <-> `uuid::Uuid` bridge
  canonicalises both directions, and
  `uniqueidentifier_round_trips_without_byte_swap` in
  `mssql_smoke.rs` guards against a regression where someone forgets
  and writes the bytes manually.
- **Streaming is materialised.** `Connection::stream` returns a
  `BufferedRowStream`. Tiberius' `QueryStream` borrows `&mut Client`
  for its lifetime, which would tie the stream to the mutex guard
  we hold around the client. The mysql driver makes the same
  trade-off; both engines advertise `Capabilities::streaming = false`
  so the TUI's progress UI can switch to "loading…" mode instead of
  rendering an incremental cursor. Real server-side cursoring
  (FAST_FORWARD) is a v2.x follow-up.

## Statement routing code review (C1) discovered four real failure modes in the
first draft of the keyword classifier. The replacement is a small
state machine:

```
classify_statement(sql) -> StatementShape
  Read  → Client::query  (preserves rows)
  Mutating  → Client::execute  (captures rows_affected)
  MutatingWithRows  → Client::query + synth rows_affected for DML+OUTPUT
```

The classifier is comment-aware (`--` and `/* … */`), CTE-aware
(peeks for INSERT/UPDATE/DELETE/MERGE inside the WITH body), and
respects single-quoted string literals and bracketed identifiers so
`'OUTPUT not a clause'` and `[OUTPUT]` do not promote a plain DML
to `MutatingWithRows`.

The routing-vs-classifier split lets the binding test suite
(`tests/mssql_binding.rs`) lock in every case in the table below
without a network round-trip; the docker-backed suite
(`tests/mssql_smoke.rs`) covers the wire-level outcome.

| Input pattern  | Shape  | `rows_affected` |
| ---------------------------------------------- | ------------------ | --------------- |
| `SELECT …` / `WITH cte AS (SELECT) SELECT …`  | `Read`  | `None`  |
| `-- comment\nINSERT …`  | `Mutating`  | `Some(n)`  |
| `INSERT … OUTPUT inserted.id VALUES (…)`  | `MutatingWithRows` | `Some(rows.len)` |
| `DELETE FROM t OUTPUT deleted.* WHERE …`  | `MutatingWithRows` | `Some(rows.len)` |
| `WITH src AS (…) INSERT INTO t SELECT …`  | `MutatingWithRows` | `None`  |
| `EXEC sp_who`  | `MutatingWithRows` | `None`  |
| `CREATE TABLE` / `DROP` / `GRANT`  | `Mutating`  | `Some(0)`  |

Downstream audit can rely on:

- `result.rows_affected.is_some` whenever the user executed a
  classic DML or a DML+OUTPUT (the latter via row count, which is
  exact for OUTPUT clauses);
- `result.rows` being non-empty whenever the engine produced rows,
  regardless of routing.

## Read-only enforcement code review (C2) flagged the original `set_read_only(true)`
as misleading: it issued `SET TRANSACTION ISOLATION LEVEL SNAPSHOT`,
returned `Ok()`, and the engine still accepted every write. The
replacement is a driver-layer guard:

1. `MssqlConnection` holds an `Arc<AtomicBool>`.
2. `set_read_only(b)` stores `b` into the atomic. `Ok()` is
  returned only after the flag is durable for subsequent reads.
3. `execute` consults the atomic *before* classify→route; any
  non-`Read` shape returns `Error::Unsupported` and the SQL never
  reaches the wire.
4. `ConnectionParams.read_only = true` from the on-disk config is
  honoured from the **first** statement (driver-construction time),
  not just after a manual `set_read_only` call.

This matches the trait contract that "the driver instructs the
database engine to refuse writes for the lifetime of the session":
the writes never reach the engine because the driver is now part of
the enforcement chain. The TUI's syntactic guard
(`narwhal_sql::guard_read_only`) remains the first layer of
defence.

## Connect-time budget code review (M1): `TcpStream::connect` to a black-holed
host previously inherited the OS-default SYN retransmit budget
(~127s on Linux), which would freeze the TUI on any typo'd hostname.
The driver now wraps the entire handshake (TCP + TLS + LOGIN7) in a
single `tokio::time::timeout`. Default 10s; override via the
`connect_timeout` option key (seconds, `> 0`). The choice is
deliberate: a `connect_timeout = 1` user wants fast-fail behaviour,
and we should respect that even though the LOGIN7 may not have
finished.

## How to extend this driver

### Adding an option key

1. Add the key to `OPTIONS_WHITELIST` in `mssql/mod.rs`.
2. Handle it inside `build_config` *before* the whitelist loop's
  final `else` (rejection path).
3. Update the table in this doc and `docs/drivers/mssql.md`.
4. Add a unit test in `mssql::tests` (`build_config_*`) that
  exercises both the accepted and rejected paths.

### Adding a new `Value` variant binding

`Param::to_sql` in `mssql/types.rs` is exhaustive over
`narwhal_core::Value`. A new variant will produce a warning today
(catch-all) and an error once `Value` becomes `#[non_exhaustive]`
in v3.x. The conventional landing pattern is:

1. Pick the right `ColumnData` arm (use `ColumnData::String` if it's
  text, `ColumnData::Binary` if it's bytes).
2. Add the matching arm in `column_to_value` so the round-trip is
  symmetric.
3. Cover both directions in `tests/mssql_smoke.rs` — pick a wire
  type the server supports natively for byte-accuracy.

## Out of scope

- **Integrated authentication on Linux** (`integrated-auth-gssapi`).
  Documented as v2.4 follow-up. Connect-time error is explicit so
  the user knows to wait or switch to SQL Auth.
- **Always Encrypted** (client-side column encryption).
- **Bulk insert** (`BCP` / `OPENROWSET`). Tiberius exposes a bulk-
  insert API but the workspace has no `Connection` method for it
  yet — it would land alongside the same shape on the PostgreSQL
  COPY path.
- **MARS** (Multiple Active Result Sets) — needs protocol-level work
  in tiberius first.
