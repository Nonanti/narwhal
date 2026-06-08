# Audit log

Every executed statement, every connection open / close, and every
authentication failure can be appended to a JSONL audit log.
Suitable as compliance evidence.

## Configure

```toml
[[audit.sinks]]
kind = "file"
path = "~/.local/share/narwhal/audit.log"
rotate = "daily"          # daily | size:<bytes> | none

[[audit.sinks]]
kind = "syslog"           # Linux only, opt-in via the `syslog` feature
facility = "user"
```

Multiple sinks fan out: every event is written to all configured
sinks.

## Event shape

```jsonc
{
  "schema_version": 1,
  "ts": "2026-06-08T14:30:12.473Z",
  "session_id": "0192b3c8-...",
  "kind": "query",
  "connection": "prod",
  "user": "berkant",
  "sql": "SELECT id FROM orders WHERE ...",
  "rows": 42,
  "ok": true,
  "duration_ms": 17,
  "source": "tui"           // tui | mcp | exec | pending
}
```

The full schema lives in `narwhal_audit::event::AuditEvent`. Fields
are additive — never breaking — so a shipper that pins
`schema_version = 1` keeps working when fields are added.

## Redaction

By default, password literals in SQL are masked. Column-level
redaction is opt-in:

```toml
[audit.redact]
passwords = true
columns = ["api_key", "ssn", "credit_card"]
```

Column matching is ASCII-case-insensitive. Non-ASCII column rules
must be byte-identical aside from ASCII case.

## Rotation

`rotate = "daily"` cuts a new file at midnight UTC and renames the
previous file with a timestamp suffix:

```
audit.log
audit.log.20260607T235959.999Z
```

`rotate = "size:104857600"` (= 100 MiB) cuts when the current file
hits the threshold.

## Tailing

```sh
narwhal audit tail               # follow the active file
narwhal audit tail --since 1h    # last hour
narwhal audit tail --kind query --connection prod
```

`--path /path/to/audit.log` overrides the default sink lookup.
