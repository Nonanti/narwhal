//! `explain_query` — return the driver-native `EXPLAIN` output for a SQL
//! statement.
//!
//! We deliberately do **not** parse the plan into a unified structure:
//! every engine emits a different shape (Postgres trees, `MySQL` row-table,
//! `SQLite` virtual-machine ops, `ClickHouse` pipeline, `DuckDB` ASCII), and
//! pre-digesting it would strip cues an LLM is excellent at reading. The
//! tool returns the raw `EXPLAIN` rows plus the dialect tag so the agent
//! knows which conventions apply.
//!
//! Safety: the underlying `EXPLAIN <statement>` is itself a read on every
//! driver narwhal supports (PG `EXPLAIN ANALYZE` *does* run the
//! statement, but we don't add `ANALYZE`). We still wrap the call in the
//! same `BEGIN ... ROLLBACK` sandwich `run_query` uses, to keep behaviour
//! consistent and protect against drivers/extensions where `EXPLAIN`
//! might have side effects.

use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;

use crate::context::ServerContext;
use crate::error::McpError;
use crate::json_value::value_to_json;
use crate::tools::{Tool, ToolOutput};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    connection: String,
    sql: String,
    /// Pass-through flags that the driver's EXPLAIN supports. Currently
    /// we only honour `analyze` for Postgres / `MySQL`, which we expose via
    /// a single boolean. Other knobs (formats, costs) come later when
    /// there is a concrete need.
    #[serde(default)]
    analyze: bool,
}

pub struct ExplainQueryTool;

#[async_trait]
impl Tool for ExplainQueryTool {
    fn name(&self) -> &'static str {
        "explain_query"
    }

    fn description(&self) -> &'static str {
        "Return the driver-native EXPLAIN output for a SQL statement. The \
         response includes the dialect tag (postgres/mysql/sqlite/duckdb/\
         clickhouse) so the agent knows how to interpret the plan. Set \
         `analyze=true` for Postgres/MySQL to actually run the statement \
         and gather real row counts — use with care, EXPLAIN ANALYZE \
         performs the work."
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
                    "description": "SQL statement to explain. Do NOT prefix with EXPLAIN — the tool adds the right dialect-specific prefix itself."
                },
                "analyze": {
                    "type": "boolean",
                    "description": "Postgres/MySQL: wrap with EXPLAIN ANALYZE so the statement actually runs and timings are real. Ignored on other engines. Default false.",
                    "default": false
                }
            },
            "required": ["connection", "sql"],
            "additionalProperties": false,
        })
    }

    async fn call(&self, ctx: &ServerContext, arguments: Value) -> Result<ToolOutput, McpError> {
        let args: Args = serde_json::from_value(arguments)
            .map_err(|e| McpError::InvalidParams(e.to_string()))?;

        // Reject statements that already start with EXPLAIN so we don't
        // accidentally produce `EXPLAIN EXPLAIN ...`.
        let trimmed = args.sql.trim_start();
        if trimmed.to_ascii_uppercase().starts_with("EXPLAIN") {
            return Ok(ToolOutput::err(
                "`sql` must not start with EXPLAIN — the tool adds the \
                 dialect-appropriate prefix automatically."
                    .to_string(),
            ));
        }

        let connection_config = ctx
            .connections()
            .connections
            .iter()
            .find(|c| c.name == args.connection)
            .cloned();
        let Some(config) = connection_config else {
            return Ok(ToolOutput::err(format!(
                "unknown connection: {}. Call `list_connections` to see valid names.",
                args.connection
            )));
        };

        let prefixed_sql = explain_prefix(&config.driver, args.analyze, &args.sql);

        // Audit before dispatch so the journal still captures the
        // attempt if the explain hangs.
        ctx.audit_query(&args.connection, &prefixed_sql, /* read_only = */ true)
            .await;

        let mut conn = match ctx.open_connection(&args.connection).await {
            Ok(c) => c,
            Err(McpError::UnknownConnection(_)) => {
                return Ok(ToolOutput::err(format!(
                    "unknown connection: {}.",
                    args.connection
                )));
            }
            Err(other) => return Err(other),
        };

        let started = Instant::now();
        // EXPLAIN ANALYZE on PG/MySQL runs the statement. Even so we keep
        // the ROLLBACK sandwich so the side effects of the analyzed
        // statement (e.g. sequence bumps, temp tables) do not persist
        // beyond this call.
        let exec_result = run_in_sandbox(conn.as_mut(), &prefixed_sql).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let _ = conn.close().await;

        let result = match exec_result {
            Ok(r) => r,
            Err(error) => {
                return Ok(ToolOutput::err(format!(
                    "explain failed on `{}`: {error}",
                    args.connection
                )));
            }
        };

        let columns: Vec<Value> = result
            .columns
            .iter()
            .map(|c| json!({"name": c.name, "type": c.data_type}))
            .collect();
        let rows: Vec<Value> = result
            .rows
            .iter()
            .map(|row| Value::Array(row.0.iter().map(value_to_json).collect()))
            .collect();

        let payload = json!({
            "connection": args.connection,
            "dialect": config.driver,
            "analyzed": args.analyze && analyze_supported(&config.driver),
            "explain_sql": prefixed_sql,
            "elapsed_ms": elapsed_ms,
            "columns": columns,
            "rows": rows,
        });

        Ok(ToolOutput::ok(
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string()),
        ))
    }
}

/// Driver-specific `EXPLAIN` prefix.
///
/// Centralised here because each engine spells the keywords slightly
/// differently. `SQLite` uses `EXPLAIN QUERY PLAN` for the high-level tree;
/// the low-level `EXPLAIN` lists VDBE opcodes and is rarely what an agent
/// wants. `ClickHouse` only accepts plain `EXPLAIN` (no ANALYZE), `MySQL`
/// accepts both, Postgres prefers the parenthesised form.
fn explain_prefix(driver: &str, analyze: bool, sql: &str) -> String {
    match driver {
        "postgres" => {
            if analyze {
                format!("EXPLAIN (ANALYZE, VERBOSE, BUFFERS) {sql}")
            } else {
                format!("EXPLAIN (VERBOSE) {sql}")
            }
        }
        "mysql" => {
            if analyze {
                format!("EXPLAIN ANALYZE {sql}")
            } else {
                format!("EXPLAIN {sql}")
            }
        }
        // SQLite's `EXPLAIN QUERY PLAN` is the agent-friendly form; the
        // raw `EXPLAIN` produces VDBE opcodes. No ANALYZE knob.
        "sqlite" => format!("EXPLAIN QUERY PLAN {sql}"),
        // DuckDB supports `EXPLAIN ANALYZE` since v0.7; pass it through.
        "duckdb" => {
            if analyze {
                format!("EXPLAIN ANALYZE {sql}")
            } else {
                format!("EXPLAIN {sql}")
            }
        }
        // ClickHouse's EXPLAIN has its own modifiers (PLAN, PIPELINE,
        // ESTIMATE). We default to PLAN which is what users want
        // 90% of the time.
        "clickhouse" => format!("EXPLAIN PLAN {sql}"),
        // Unknown driver: fall back to plain EXPLAIN and hope for the
        // best. The driver itself will reject if the syntax is wrong.
        _ => format!("EXPLAIN {sql}"),
    }
}

fn analyze_supported(driver: &str) -> bool {
    matches!(driver, "postgres" | "mysql" | "duckdb")
}

/// `BEGIN ... ROLLBACK` sandwich, copy-pasted from `run_query` to keep
/// the two tools independent. If we add a third user we'll extract it
/// into a small helper module.
async fn run_in_sandbox(
    conn: &mut dyn narwhal_core::Connection,
    sql: &str,
) -> Result<narwhal_core::QueryResult, narwhal_core::Error> {
    if let Err(error) = conn.begin().await {
        warn!(%error, "explain sandbox: BEGIN failed, executing without transaction");
        return conn.execute(sql, &[]).await;
    }
    let result = conn.execute(sql, &[]).await;
    if let Err(error) = conn.rollback().await {
        warn!(%error, "explain sandbox: ROLLBACK after explain failed");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_prefix_uses_parens_and_verbose() {
        let s = explain_prefix("postgres", false, "SELECT 1");
        assert_eq!(s, "EXPLAIN (VERBOSE) SELECT 1");
    }

    #[test]
    fn pg_analyze_includes_buffers() {
        let s = explain_prefix("postgres", true, "SELECT 1");
        assert_eq!(s, "EXPLAIN (ANALYZE, VERBOSE, BUFFERS) SELECT 1");
    }

    #[test]
    fn sqlite_uses_query_plan_form() {
        let s = explain_prefix("sqlite", false, "SELECT 1");
        assert_eq!(s, "EXPLAIN QUERY PLAN SELECT 1");
        // ANALYZE is ignored on sqlite — same prefix.
        let s2 = explain_prefix("sqlite", true, "SELECT 1");
        assert_eq!(s2, "EXPLAIN QUERY PLAN SELECT 1");
    }

    #[test]
    fn clickhouse_uses_plan_modifier() {
        assert_eq!(
            explain_prefix("clickhouse", false, "SELECT 1"),
            "EXPLAIN PLAN SELECT 1"
        );
    }

    #[test]
    fn unknown_driver_falls_back_to_plain_explain() {
        assert_eq!(
            explain_prefix("snowflake", false, "SELECT 1"),
            "EXPLAIN SELECT 1"
        );
    }

    #[test]
    fn analyze_support_is_engine_specific() {
        assert!(analyze_supported("postgres"));
        assert!(analyze_supported("mysql"));
        assert!(analyze_supported("duckdb"));
        assert!(!analyze_supported("sqlite"));
        assert!(!analyze_supported("clickhouse"));
    }
}
