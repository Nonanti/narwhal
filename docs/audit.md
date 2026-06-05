# Audit log (v2.0+)

narwhal v2.0 ships an opt-in **append-only JSONL audit log**. When
enabled, every executed query, connection lifecycle event, plugin
load, and on-disk configuration change is projected as a self-contained
JSON object on its own line, fanned out to one or more sinks
(filesystem, stdout, syslog).

The feature is designed for SOC 2 CC7.2 / ISO 27001 A.12.4 evidence
collection: turn it on, point a log shipper at the JSONL file, and the
TUI produces the same wire format whether the user clicked through the
sidebar, ran `:run`, or fired a request through the MCP server.

## TL;DR

```toml
# ~/.config/narwhal/settings.toml
[audit]
enabled        = true
sinks          = ["file:/var/log/narwhal/audit-%Y-%m-%d.jsonl"]
redact_passwords = true
redact_columns = ["ssn", "card_number"]
block_on_full  = false
```

```bash
narwhal             # run as normal — audit emits in the background
narwhal audit tail -f --kind query   # follow the tail; filter to queries
```

## Configuration

The block lives under `[audit]` in `settings.toml` (schema v2):

| Field | Default | Notes |
|---|---|---|
| `enabled` | `false` | Master switch. When `false`, every emit site short-circuits on a single atomic load. |
| `sinks` | `[]` | One or more sink specs. Each event lands on every configured sink. |
| `redact_passwords` | `true` | Apply [`narwhal_history::redact_sql_secrets`] to the SQL of every `Query` event. |
| `redact_columns` | `[]` | Extra column names whose values are masked (case-insensitive). |
| `block_on_full` | `false` | When `true`, query dispatch *blocks* if the audit channel is full. See [Back-pressure](#back-pressure). |

### Sink specs

Sink entries are plain strings:

- `file:<path>` — append to a path. Path may contain strftime tokens
  (`%Y`, `%m`, `%d`, `%H`, …) which are expanded **at open time** and
  again whenever the resolved name changes — i.e. daily file rotation
  is automatic with `audit-%Y-%m-%d.jsonl`.
- `stdout` — write each line to the binary's stdout. Useful for
  ephemeral debug runs and container deployments that already collect
  stdout.
- `syslog` — forward to the local syslog daemon (Linux only).
  Requires building with the `audit-syslog` feature
  (`cargo install narwhaldb --features audit-syslog`).

Sinks fan out independently. A failure on one is logged via
`tracing::warn` and does **not** stop the others.

## Wire format

Every line is a single JSON object terminated by a newline. The envelope
adds two fields applied by the sink:

```json
{
  "schema_version": 1,
  "time": "2026-06-04T10:00:00.123Z",
  "kind": "query",
  "session_id": "5b1c…",
  "sql": "SELECT * FROM accounts WHERE id = $1",
  "params": ["42"],
  "rows": 1,
  "elapsed_ms": 7,
  "succeeded": true,
  "error": null
}
```

### Event kinds

| `kind` | Trigger | Carries |
|---|---|---|
| `connection_opened` | `:open <name>` / URL open / re-open. | `conn`, `user`, `host`, `session_id` |
| `connection_closed` | `:close` or session drop. | `session_id`, `duration_ms` |
| `query` | Every successful or failed statement, including MCP. | `session_id`, `sql` (post-redaction), `params`, `rows`, `elapsed_ms`, `succeeded`, `error` |
| `configuration` | Wizard commit, connection remove, credential rotate. | `change`, `by` |
| `plugin_loaded` | Lua plugin loaded (`:plug-load` or auto-load). | `plugin`, `version`, `capabilities` |

`schema_version` is bumped only when an existing variant changes its
shape; adding a new variant is non-breaking. Consumers branch on
`schema_version` to handle future migrations without ambiguity.

## CLI: `narwhal audit tail`

A read-only inspector that never mutates the file:

```bash
narwhal audit tail                       # dump the whole file
narwhal audit tail -n 200                # last 200 lines
narwhal audit tail -f                    # start at EOF and follow
narwhal audit tail -f -n 50              # last 50, then follow
narwhal audit tail --kind connection_opened   # filter by event kind
narwhal audit tail --path /var/log/narwhal/audit.jsonl   # explicit file
```

Without `--path`, the resolver picks the first `file:` sink from
`settings.audit.sinks` and re-runs the strftime expansion against UTC
`now` — the same logic the runtime uses — so today's daily file is
selected automatically.

## Back-pressure

A bounded `tokio::sync::mpsc` channel sits between the emit sites and
the sink worker. Default behaviour: **lossy** — when the channel is
full, the emit site drops the event and bumps a `tracing::warn`
counter. Query dispatch is never blocked.

For compliance-first deployments where dropping a line is unacceptable
(e.g. SOC 2 CC7.2 evidence), set:

```toml
[audit]
block_on_full = true
```

…and dispatch will then `await` the channel. This trades responsiveness
for completeness: a slow sink (full disk, syslog daemon down) will
freeze the editor until it recovers. We recommend pairing
`block_on_full = true` with **at least one fast sink** (`file:` on a
local SSD) so the worker rarely backs up.

## Redaction

Redaction is **best-effort, not a security boundary.** The audit log
should be treated as containing sensitive data; protect it with
filesystem ACLs (mode `0640`, dedicated `narwhal-audit` group) and
shipping policies, not with redaction.

The redactor performs two passes:

1. **Password masking** — when `redact_passwords = true`, the SQL text
   of every `Query` event passes through
   [`narwhal_history::redact_sql_secrets`], the same routine the query
   history journal uses. It masks `PASSWORD '…'`, `IDENTIFIED BY '…'`,
   and a handful of dialect-specific shapes.
2. **Column-name masking** — when `redact_columns = ["ssn"]` is set,
   bound parameter values whose label matches the list are replaced
   with `***`.

Caveats:

- The masker is **regex-based**. Cleverly-quoted or computed secrets
  may evade it.
- Bind parameter values are stored stringified; a complex value type
  may serialise in a way that doesn't trigger the column-name mask.
- Errors returned by the driver are appended verbatim; some drivers
  echo the offending SQL in error messages.

When in doubt, treat the audit file as confidential at the same
classification level as a database password dump.

## Operational guidance

### File ownership

```bash
sudo install -d -m 0750 -o narwhal -g narwhal-audit /var/log/narwhal
sudo touch /var/log/narwhal/audit.jsonl
sudo chown narwhal:narwhal-audit /var/log/narwhal/audit.jsonl
sudo chmod 0640 /var/log/narwhal/audit.jsonl
```

The file sink writes O_APPEND, so multiple processes appending to the
same file remain interleaved at line boundaries (POSIX guarantees
atomic writes up to `PIPE_BUF`, well above any single JSON line we
emit).

### Rotation

The file sink rotates automatically when the resolved strftime path
changes — i.e. a daily-rotating template (`audit-%Y-%m-%d.jsonl`) opens
a new file each UTC midnight without process restart. External
log-rotation (logrotate, vector) is unnecessary and may corrupt the
output by truncating mid-line.

### Shipping

Any log shipper that ingests JSONL works. Useful queries:

```bash
# every failed query in the last 24 h
narwhal audit tail -n 100000 --kind query | jq 'select(.succeeded == false)'

# all connections opened against prod
narwhal audit tail --kind connection_opened | jq 'select(.host | startswith("db.prod"))'
```

## Compliance mapping

| Framework | Control | Coverage |
|---|---|---|
| SOC 2 | CC7.2 (System Operations: Monitor) | `query`, `connection_*` events provide tamper-evident usage trail. |
| SOC 2 | CC6.1 (Logical Access) | `connection_opened` records authenticated `user` + `host`. |
| ISO 27001 | A.12.4.1 (Event logging) | JSONL append-only with rotation. |
| ISO 27001 | A.12.4.3 (Administrator logs) | `configuration` events cover wizard / vault rotation. |
| PCI-DSS | Req. 10.2 (Audit trails) | Full statement text + `succeeded` flag. Pair with `redact_columns = ["pan", "cvv"]`. |
| HIPAA | §164.312(b) (Audit controls) | Same JSONL trail covers PHI access through `query` events. |

These mappings are guidance, not certification. Whether the trail
*satisfies* a particular auditor depends on local retention, access
control, and integrity guarantees outside the application's scope —
file-system encryption at rest, write-once storage if required, etc.

## Limitations

- **No HMAC or hash chain.** Lines are not tamper-evident at the
  application layer; rely on the underlying storage (immutable
  bucket, WORM filesystem) for that.
- **MCP `query` events** carry the same `session_id` as the
  originating TUI session when the MCP server is embedded in the
  same process; standalone MCP runs produce their own correlation
  ids.
- **Bind parameters** are not threaded through the `Query` event yet
  — the field exists on the wire format but is currently empty for
  every event. A future change will populate it without bumping
  `schema_version`.
- **Plugin version field** is set to `"lua"` for Lua plugins; WASM
  plugins fill the real `version` declared in the manifest.

## Implementation notes

The audit subsystem lives in [`crates/narwhal-audit`](../crates/narwhal-audit).
Key entry points:

- `AuditService` — orchestrator. `emit(event)` is the only public
  method; the worker is internal.
- `AuditSink` trait — implementations: `FileSink`, `StdoutSink`,
  `SyslogSink`.
- `Redactor` — pre-render mask pass.
- `AuditEvent` — wire format. `#[non_exhaustive]` so new variants
  don't break consumers built against earlier minor versions.

See `narwhal-audit/src/lib.rs` for the architectural commentary.
