//! Parse and render PostgreSQL `EXPLAIN (ANALYZE, FORMAT JSON)` output.
//!
//! Only PostgreSQL is supported. The driver returns a single-row,
//! single-column result whose value is a JSON array containing one plan
//! object. The plan tree lives under `Plan.Plans` and is rendered as
//! indented lines that mimic `EXPLAIN`'s textual format while preserving
//! actual-time and row counts collected by `ANALYZE`.

use serde_json::Value as Json;

/// Wraps `sql` so that the engine emits an analysable JSON plan.
///
/// The wrapper expects the input to be a single statement without a
/// trailing semicolon; [`crate::commands::parse`] already strips it.
pub fn wrap_explain(sql: &str) -> String {
    format!("EXPLAIN (ANALYZE, VERBOSE, BUFFERS, FORMAT JSON) {sql}")
}

/// Single rendered line of an EXPLAIN plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainLine {
    pub depth: usize,
    pub text: String,
}

/// Parsed plan ready for rendering.
#[derive(Debug, Clone, Default)]
pub struct ExplainPlan {
    pub lines: Vec<ExplainLine>,
    pub planning_time_ms: Option<f64>,
    pub execution_time_ms: Option<f64>,
}

/// Parse the JSON document returned by `EXPLAIN (FORMAT JSON)`.
pub fn parse(json_text: &str) -> Result<ExplainPlan, String> {
    let root: Json = serde_json::from_str(json_text).map_err(|e| e.to_string())?;
    let array = root
        .as_array()
        .ok_or_else(|| "expected a top-level JSON array".to_owned())?;
    let entry = array
        .first()
        .ok_or_else(|| "EXPLAIN output is empty".to_owned())?;

    let Some(root_plan) = entry.get("Plan") else {
        return Err("EXPLAIN output is missing a Plan node".to_owned());
    };
    let mut lines = Vec::new();
    walk(root_plan, 0, &mut lines);
    Ok(ExplainPlan {
        lines,
        planning_time_ms: entry.get("Planning Time").and_then(Json::as_f64),
        execution_time_ms: entry.get("Execution Time").and_then(Json::as_f64),
    })
}

fn walk(node: &Json, depth: usize, out: &mut Vec<ExplainLine>) {
    let header = format_node_header(node);
    out.push(ExplainLine {
        depth,
        text: header,
    });
    for detail in collect_details(node) {
        out.push(ExplainLine {
            depth: depth + 1,
            text: detail,
        });
    }
    if let Some(children) = node.get("Plans").and_then(Json::as_array) {
        for child in children {
            walk(child, depth + 1, out);
        }
    }
}

fn format_node_header(node: &Json) -> String {
    let node_type = node
        .get("Node Type")
        .and_then(Json::as_str)
        .unwrap_or("Unknown");
    let mut s = node_type.to_owned();

    if let Some(rel) = node.get("Relation Name").and_then(Json::as_str) {
        s.push_str(" on ");
        s.push_str(rel);
        if let Some(alias) = node.get("Alias").and_then(Json::as_str) {
            if alias != rel {
                s.push(' ');
                s.push_str(alias);
            }
        }
    } else if let Some(idx) = node.get("Index Name").and_then(Json::as_str) {
        s.push_str(" using ");
        s.push_str(idx);
    }

    let plan_rows = node.get("Plan Rows").and_then(Json::as_f64).unwrap_or(0.0);
    let plan_width = node.get("Plan Width").and_then(Json::as_i64).unwrap_or(0);
    let startup = node
        .get("Startup Cost")
        .and_then(Json::as_f64)
        .unwrap_or(0.0);
    let total = node.get("Total Cost").and_then(Json::as_f64).unwrap_or(0.0);
    use std::fmt::Write as _;
    let _ = write!(
        &mut s,
        "  (cost={startup:.2}..{total:.2} rows={plan_rows:.0} width={plan_width})"
    );

    if let (Some(actual_startup), Some(actual_total), Some(actual_rows), Some(loops)) = (
        node.get("Actual Startup Time").and_then(Json::as_f64),
        node.get("Actual Total Time").and_then(Json::as_f64),
        node.get("Actual Rows").and_then(Json::as_f64),
        node.get("Actual Loops").and_then(Json::as_f64),
    ) {
        let _ = write!(
            &mut s,
            "  (actual={actual_startup:.3}..{actual_total:.3} rows={actual_rows:.0} loops={loops:.0})"
        );
    }

    s
}

fn collect_details(node: &Json) -> Vec<String> {
    let mut out = Vec::new();
    let push_str = |out: &mut Vec<String>, label: &str, key: &str| {
        if let Some(v) = node.get(key).and_then(Json::as_str) {
            out.push(format!("{label}: {v}"));
        }
    };
    let push_array_str = |out: &mut Vec<String>, label: &str, key: &str| {
        if let Some(arr) = node.get(key).and_then(Json::as_array) {
            let joined: Vec<&str> = arr.iter().filter_map(Json::as_str).collect();
            if !joined.is_empty() {
                out.push(format!("{label}: {}", joined.join(", ")));
            }
        }
    };
    push_array_str(&mut out, "Output", "Output");
    push_str(&mut out, "Filter", "Filter");
    push_str(&mut out, "Index Cond", "Index Cond");
    push_str(&mut out, "Hash Cond", "Hash Cond");
    push_str(&mut out, "Join Filter", "Join Filter");
    push_str(&mut out, "Sort Key", "Sort Key");
    if let Some(rows_removed) = node.get("Rows Removed by Filter").and_then(Json::as_f64) {
        if rows_removed > 0.0 {
            out.push(format!("Rows Removed by Filter: {rows_removed:.0}"));
        }
    }
    if let Some(read) = node.get("Shared Read Blocks").and_then(Json::as_f64) {
        if read > 0.0 {
            let hit = node
                .get("Shared Hit Blocks")
                .and_then(Json::as_f64)
                .unwrap_or(0.0);
            out.push(format!("Buffers: shared hit={hit:.0} read={read:.0}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[
        {
            "Plan": {
                "Node Type": "Limit",
                "Startup Cost": 0.00,
                "Total Cost": 0.04,
                "Plan Rows": 1,
                "Plan Width": 36,
                "Actual Startup Time": 0.012,
                "Actual Total Time": 0.013,
                "Actual Rows": 1,
                "Actual Loops": 1,
                "Plans": [
                    {
                        "Node Type": "Seq Scan",
                        "Relation Name": "events",
                        "Alias": "events",
                        "Startup Cost": 0.00,
                        "Total Cost": 3.40,
                        "Plan Rows": 200,
                        "Plan Width": 36,
                        "Actual Startup Time": 0.005,
                        "Actual Total Time": 0.025,
                        "Actual Rows": 200,
                        "Actual Loops": 1,
                        "Filter": "(kind = 'click'::text)",
                        "Rows Removed by Filter": 0
                    }
                ]
            },
            "Planning Time": 0.123,
            "Execution Time": 0.456
        }
    ]"#;

    #[test]
    fn parses_sample_plan() {
        let plan = parse(SAMPLE).unwrap();
        assert_eq!(plan.planning_time_ms, Some(0.123));
        assert_eq!(plan.execution_time_ms, Some(0.456));
        assert!(plan.lines.len() >= 3);
        assert_eq!(plan.lines[0].depth, 0);
        assert!(plan.lines[0].text.starts_with("Limit"));
        assert!(plan.lines[0].text.contains("cost=0.00..0.04"));
        assert!(plan.lines[0].text.contains("actual=0.012..0.013"));
        // Seq Scan should be deeper than Limit
        let seq_scan_line = plan
            .lines
            .iter()
            .find(|l| l.text.contains("Seq Scan"))
            .unwrap();
        assert!(seq_scan_line.depth > 0);
        assert!(seq_scan_line.text.contains("on events"));
        assert!(plan.lines.iter().any(|l| l.text.contains("Filter:")));
    }

    #[test]
    fn wrap_prefixes_explain() {
        let wrapped = wrap_explain("SELECT 1");
        assert!(wrapped.starts_with("EXPLAIN (ANALYZE"));
        assert!(wrapped.ends_with("SELECT 1"));
    }

    #[test]
    fn parse_rejects_non_array() {
        assert!(parse("{}").is_err());
        assert!(parse("[]").is_err());
        assert!(parse("not json").is_err());
    }
}
