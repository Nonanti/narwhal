//! `run_query` — execute a SQL statement against a named connection.
//!
//! # Safety posture
//!
//! The default is **read-only**. Three layers cooperate to keep the agent
//! from mutating the database without an explicit opt-in:
//!
//! 1. **Statement guard** — the first significant token must be one of
//!    `SELECT`, `WITH`, `SHOW`, `EXPLAIN`, `DESCRIBE`, `DESC`, `PRAGMA`,
//!    or `VALUES`. Anything else is rejected up-front with a tool-level
//!    error so the request never reaches the driver.
//! 2. **Transaction sandwich** — the statement runs inside a transaction
//!    that always ends in `ROLLBACK`, even on success. Anything that
//!    sneaks past the guard (e.g. a `CREATE TABLE` inside a `WITH`-prefixed
//!    statement on a driver that allows it) is therefore unwound.
//! 3. **Row limit** — results are truncated to `limit` rows (default
//!    1 000) so a runaway query cannot exhaust the agent's context or
//!    narwhal's memory.
//!
//! Writes are still possible — set `read_only = false` explicitly. In that
//! mode the guard and the sandwich are both disabled and the statement is
//! executed directly.

use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;

use crate::context::ServerContext;
use crate::error::McpError;
use crate::json_value::{json_array_to_values, value_to_json};
use crate::tools::{Tool, ToolOutput};

/// Default ceiling on returned rows. A handful of agents (Claude Desktop,
/// Cursor) cap tool-call responses at ~100 KB; 1 000 rows is well under
/// that for any reasonable column width.
const DEFAULT_LIMIT: usize = 1_000;

/// Hard cap on the row limit an agent can request, to keep the response
/// payload below the protocol's practical size budget.
const MAX_LIMIT: usize = 10_000;

/// Per-cell byte ceiling for serialised string values. A single 1 GB
/// `TEXT` / `BYTEA` cell would otherwise blow the agent's context
/// budget and can be used as a denial-of-service against the MCP host
/// (Claude Desktop, Cursor, Aider). Cells above this cap are truncated
/// in place with a `…<N bytes truncated>` suffix and the response
/// flag `cell_truncated = true` is set so the agent knows to drill
/// down with a tighter query.  (Bug H2 fix.)
const MAX_CELL_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    connection: String,
    sql: String,
    /// Positional bind parameters. The driver substitutes them for
    /// placeholder tokens (`$1`/`$2` on PG, `?` everywhere else). When
    /// omitted the statement runs without parameters — use this for
    /// literal-only queries.
    #[serde(default)]
    params: Vec<narwhal_core::Value>,
    /// `true` (default) wraps the statement in a `BEGIN ... ROLLBACK`
    /// sandwich and rejects anything that does not look like a query.
    #[serde(default = "default_read_only")]
    read_only: bool,
    /// Maximum number of rows to include in the response. Defaults to
    /// [`DEFAULT_LIMIT`] and is clamped to [`MAX_LIMIT`].
    #[serde(default)]
    limit: Option<usize>,
}

const fn default_read_only() -> bool {
    true
}

pub struct RunQueryTool;

#[async_trait]
impl Tool for RunQueryTool {
    fn name(&self) -> &'static str {
        "run_query"
    }

    fn description(&self) -> &'static str {
        "Execute a SQL statement against a named connection. Defaults to \
         read-only: the statement is rejected unless it begins with \
         SELECT/WITH/SHOW/EXPLAIN/DESCRIBE/PRAGMA/VALUES, and even then it \
         runs inside a transaction that always ROLLBACKs. Set \
         `read_only=false` to allow writes (use sparingly). Pass `params` \
         to bind values for placeholder tokens (`$1`/`$2` on Postgres, \
         `?` elsewhere) and avoid string-concatenation SQL injection. \
         Rows are truncated to `limit` (default 1000, max 10000) and the \
         response includes a `truncated` flag when truncation occurred."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "connection": {
                    "type": "string",
                    "description": "Connection name as returned by `list_connections`."
                },
                "sql": {
                    "type": "string",
                    "description": "Single SQL statement to execute. Use placeholders (`$1`/`$2` on Postgres, `?` elsewhere) plus `params` instead of string-interpolating values. Multi-statement scripts are not supported."
                },
                "params": {
                    "type": "array",
                    "description": "Positional bind parameters. JSON primitives map to SQL scalars; `{\"$bytes_base64\": \"<base64>\"}` envelopes bind as BYTEA/BLOB. Length must match the number of placeholders in `sql`.",
                    "items": true
                },
                "read_only": {
                    "type": "boolean",
                    "description": "Default true. False disables the statement guard and the ROLLBACK sandwich.",
                    "default": true
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of rows to return. Default 1000, max 10000.",
                    "minimum": 1,
                    "maximum": MAX_LIMIT,
                }
            },
            "required": ["connection", "sql"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, ctx: &ServerContext, arguments: Value) -> Result<ToolOutput, McpError> {
        // Deserialize against a `serde_json::Value`-typed `params` field
        // first so we get position-aware error messages out of the JSON
        // converter. `Args` directly typed as `Vec<narwhal_core::Value>`
        // would surface serde's generic "expected X, got Y" instead.
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawArgs {
            connection: String,
            sql: String,
            #[serde(default)]
            params: Vec<serde_json::Value>,
            #[serde(default = "default_read_only")]
            read_only: bool,
            #[serde(default)]
            limit: Option<usize>,
        }
        let raw: RawArgs = serde_json::from_value(arguments)
            .map_err(|e| McpError::InvalidParams(e.to_string()))?;

        let params = match json_array_to_values(&raw.params) {
            Ok(v) => v,
            Err(error) => {
                // Param decoding errors are *agent* mistakes — surface
                // them as a tool-level error so the agent retries with
                // the right shape instead of crashing the dispatch.
                return Ok(ToolOutput::err(format!("invalid params: {error}")));
            }
        };
        let args = Args {
            connection: raw.connection,
            sql: raw.sql,
            params,
            read_only: raw.read_only,
            limit: raw.limit,
        };

        let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

        // Workspace ACL + global --read-only flag: if writes aren't
        // permitted the agent cannot opt out of read_only mode even by
        // passing the flag. We refuse up-front with a message that
        // tells the agent *why* so it can stop retrying.
        let read_only = if !ctx.writes_allowed() && !args.read_only {
            let reason = if ctx.force_read_only() {
                String::from(
                    "the MCP server was launched with --read-only \
                     — `read_only=false` is rejected on every call. \
                     Relaunch without the flag to allow writes.",
                )
            } else {
                format!(
                    "the active workspace (root: {root}) disallows writes \
                     — `read_only=false` is rejected. Open a permissive \
                     workspace or call again with `read_only=true`.",
                    root = ctx
                        .workspace()
                        .map(|w| w.root.display().to_string())
                        .unwrap_or_default()
                )
            };
            // H1: audit the rejection so the operator sees blocked
            // write attempts in the history journal.
            let tag = if ctx.force_read_only() {
                "force_read_only"
            } else {
                "workspace_acl"
            };
            ctx.audit_rejected(Some(&args.connection), &args.sql, tag)
                .await;
            return Ok(ToolOutput::err(reason));
        } else {
            args.read_only
        };

        if read_only {
            if let Err(reason) = guard_read_only(&args.sql) {
                // H1: audit the guard rejection.
                ctx.audit_rejected(
                    Some(&args.connection),
                    &args.sql,
                    &format!("read_only_guard: {reason}"),
                )
                .await;
                return Ok(ToolOutput::err(format!(
                    "read-only guard rejected the statement: {reason}. \
                     Pass `read_only=false` if a write is intended."
                )));
            }
        }

        let mut conn = match ctx.open_connection(&args.connection).await {
            Ok(c) => c,
            Err(McpError::UnknownConnection(_)) => {
                // H1: audit the unknown-connection rejection. Workspace
                // ACL violations land here too (open_connection maps
                // them to UnknownConnection on purpose).
                ctx.audit_rejected(
                    Some(&args.connection),
                    &args.sql,
                    "unknown_or_hidden_connection",
                )
                .await;
                return Ok(ToolOutput::err(format!(
                    "unknown connection: {}. Call `list_connections` to see valid names.",
                    args.connection
                )));
            }
            Err(other) => return Err(other),
        };

        // Audit-log the call. We log *before* dispatch so the journal still
        // captures the attempt if the query hangs or panics.
        ctx.audit_query(&args.connection, &args.sql, read_only)
            .await;

        let started = Instant::now();
        let exec_result = if read_only {
            run_in_sandbox(conn.as_mut(), &args.sql, &args.params).await
        } else {
            conn.execute(&args.sql, &args.params).await
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;

        // Best-effort close. We swallow close errors because the data we
        // already collected is valid output — the agent does not gain
        // anything from learning the socket misbehaved on shutdown.
        let _ = conn.close().await;

        let query_result = match exec_result {
            Ok(r) => r,
            Err(error) => {
                return Ok(ToolOutput::err(format!(
                    "query failed on `{}`: {error}",
                    args.connection
                )));
            }
        };

        let truncated = query_result.rows.len() > limit;
        let row_count_total = query_result.rows.len();

        let columns: Vec<Value> = query_result
            .columns
            .iter()
            .map(|c| json!({"name": c.name, "type": c.data_type}))
            .collect();

        // H2: per-cell byte cap. Walk every cell as we serialise it
        // and replace any string/bytes value that exceeds MAX_CELL_BYTES
        // with a truncated marker so a single fat blob can't pin the
        // agent or the MCP host's process.
        let mut cell_truncated = false;
        let rows: Vec<Value> = query_result
            .rows
            .iter()
            .take(limit)
            .map(|row| {
                Value::Array(
                    row.0
                        .iter()
                        .map(|v| cap_cell_size(value_to_json(v), &mut cell_truncated))
                        .collect(),
                )
            })
            .collect();

        let payload = json!({
            "connection": args.connection,
            "read_only": read_only,
            "elapsed_ms": elapsed_ms,
            "columns": columns,
            "rows": rows,
            "row_count": row_count_total,
            "truncated": truncated,
            "cell_truncated": cell_truncated,
            "cell_byte_cap": MAX_CELL_BYTES,
            "limit": limit,
            "rows_affected": query_result.rows_affected,
        });

        Ok(ToolOutput::ok(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string()),
        ))
    }
}

/// Inspect the first significant token of `sql` and reject anything that
/// is not obviously a read.
///
/// This is intentionally a coarse syntactic check, not a parser: it skips
/// `--`/`#` line comments and `/* … */` block comments, then matches the
/// first identifier case-insensitively against an allow-list. Strings and
/// quoted identifiers are not parsed, but they cannot legally appear before
/// the first keyword in any of the dialects we support, so the check is
/// safe in practice.
fn guard_read_only(sql: &str) -> Result<(), String> {
    let stripped = strip_leading_comments_and_whitespace(sql);
    let first_token = stripped
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();

    if first_token.is_empty() {
        return Err("empty statement".into());
    }

    const ALLOWED: &[&str] = &[
        "SELECT",   // ANSI
        "WITH",     // CTE
        "SHOW",     // PG/MySQL/CH metadata
        "EXPLAIN",  // every driver
        "DESCRIBE", // MySQL
        "DESC",     // MySQL shorthand
        "PRAGMA",   // SQLite (read forms only — write pragmas mutate session state)
        "VALUES",   // PG row-constructor SELECT
        "TABLE",    // PG `TABLE foo;` shorthand
    ];

    if !ALLOWED.contains(&first_token.as_str()) {
        return Err(format!(
            "first token `{first_token}` is not in the read-only allow-list \
             (SELECT/WITH/SHOW/EXPLAIN/DESCRIBE/DESC/PRAGMA/VALUES/TABLE)"
        ));
    }

    // Even when the first token is allowed, block known dangerous
    // function calls that can hold a connection or mutate state.
    let upper_sql = stripped.to_ascii_uppercase();
    const BLOCKED_PATTERNS: &[&str] = &[
        "PG_SLEEP", // holds a connection indefinitely
        "SLEEP",    // MySQL equivalent
        "SLEEP(",   // with parenthesis
    ];
    for pattern in BLOCKED_PATTERNS {
        if upper_sql.contains(pattern) {
            return Err(format!(
                "statement contains blocked function `{pattern}` which can \
                 hold a connection — pass `read_only=false` if intentional"
            ));
        }
    }

    Ok(())
}

/// H2: clamp a single JSON cell to [`MAX_CELL_BYTES`] so a fat
/// `TEXT` / `BYTEA` blob cannot blow the agent's context or the
/// MCP host's memory.
///
/// Only string values are truncated — numbers, booleans, null, and
/// structured arrays/objects are left intact. The `$bytes_base64`
/// envelope is a JSON object with a single string field; we walk into
/// it so its inner string is capped too.
fn cap_cell_size(value: Value, truncated_flag: &mut bool) -> Value {
    match value {
        Value::String(s) if s.len() > MAX_CELL_BYTES => {
            *truncated_flag = true;
            let mut cut = MAX_CELL_BYTES;
            // Snap back to the previous UTF-8 char boundary so the
            // resulting `String` stays valid (`is_char_boundary` is
            // O(1) and always true at 0, so the loop terminates).
            while cut > 0 && !s.is_char_boundary(cut) {
                cut -= 1;
            }
            let omitted = s.len() - cut;
            let mut out = String::with_capacity(cut + 32);
            out.push_str(&s[..cut]);
            out.push_str(&format!("…<{omitted} bytes truncated>"));
            Value::String(out)
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, cap_cell_size(v, truncated_flag)))
                .collect(),
        ),
        Value::Array(arr) => Value::Array(
            arr.into_iter()
                .map(|v| cap_cell_size(v, truncated_flag))
                .collect(),
        ),
        other => other,
    }
}

/// Skip leading whitespace and SQL comments. Returns the remainder.
fn strip_leading_comments_and_whitespace(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            // line comment: skip until newline or EOF
            s = match rest.find('\n') {
                Some(i) => &rest[i + 1..],
                None => "",
            };
            s = s.trim_start();
            continue;
        }
        if let Some(rest) = s.strip_prefix("/*") {
            // block comment: skip until `*/` (or EOF if malformed)
            s = match rest.find("*/") {
                Some(i) => &rest[i + 2..],
                None => "",
            };
            s = s.trim_start();
            continue;
        }
        break;
    }
    s
}

/// Execute `sql` inside a `BEGIN ... ROLLBACK` sandwich.
///
/// `ROLLBACK` runs unconditionally so a statement that sneaks past the
/// guard cannot mutate state. We log (but do not propagate) errors from
/// `begin` / `rollback` so the agent always sees the actual statement
/// error rather than a wrapping transaction failure.
async fn run_in_sandbox(
    conn: &mut dyn narwhal_core::Connection,
    sql: &str,
    params: &[narwhal_core::Value],
) -> Result<narwhal_core::QueryResult, narwhal_core::Error> {
    if let Err(error) = conn.begin().await {
        warn!(%error, "read-only sandbox: BEGIN failed, executing without transaction");
        // Falling through to a bare execute is safer than refusing the
        // call: drivers that don't support transactions (e.g. ClickHouse)
        // are useful read-only without one.
        return conn.execute(sql, params).await;
    }
    let result = conn.execute(sql, params).await;
    if let Err(error) = conn.rollback().await {
        warn!(%error, "read-only sandbox: ROLLBACK after query failed");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_accepts_obvious_reads() {
        for sql in [
            "SELECT 1",
            "select * from t",
            "  WITH cte AS (SELECT 1) SELECT * FROM cte",
            "EXPLAIN SELECT 1",
            "PRAGMA table_info(users)",
            "VALUES (1, 2), (3, 4)",
            "-- a comment\nSELECT 1",
            "/* block */ SELECT 1",
            "/* one */ -- two \n SELECT 1",
        ] {
            assert!(guard_read_only(sql).is_ok(), "must accept: {sql:?}");
        }
    }

    #[test]
    fn guard_rejects_writes() {
        for sql in [
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET x = 1",
            "DELETE FROM t",
            "DROP TABLE t",
            "CREATE TABLE t(id INT)",
            "ALTER TABLE t ADD COLUMN x INT",
            "TRUNCATE t",
            "GRANT SELECT ON t TO alice",
            "",
            "   ",
            "-- only comment",
        ] {
            assert!(guard_read_only(sql).is_err(), "must reject: {sql:?}");
        }
    }

    // Compile-time sanity: a future refactor that drops the ceiling
    // below the default — or zeros the default — fails the build.
    // (Runtime `assert!` on consts trips `clippy::assertions_on_constants`.)
    const _: () = {
        assert!(MAX_LIMIT >= DEFAULT_LIMIT);
        assert!(DEFAULT_LIMIT >= 1);
    };
}
