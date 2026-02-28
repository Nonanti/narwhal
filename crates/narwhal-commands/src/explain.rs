//! Parse and render `PostgreSQL` `EXPLAIN (ANALYZE, FORMAT JSON)` output.
//!
//! Only `PostgreSQL` is supported. The driver returns a single-row,
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

/// v1.1 #3: structured per-node metrics extracted from the EXPLAIN
/// JSON. Drives the tree visualiser — cost bars, row
/// estimate-vs-actual divergence highlighting, and the hot-path
/// classification.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExplainNode {
    /// `"Seq Scan"`, `"Hash Join"`, … (the JSON "Node Type" verbatim).
    pub node_type: String,
    /// `Some("users")` for scan nodes that target a relation.
    pub relation: Option<String>,
    /// `Some("users_pkey")` for index nodes.
    pub index: Option<String>,
    /// Optional filter expression — used by the renderer as a hint
    /// when divergent rows show up.
    pub filter: Option<String>,
    /// Planner cost estimate — inclusive total. Drives the cost bar
    /// width.
    pub total_cost: f64,
    /// Planner cost estimate — startup only.
    pub startup_cost: f64,
    /// Estimated rows.
    pub plan_rows: f64,
    /// Actual rows returned (only present with `ANALYZE`).
    pub actual_rows: Option<f64>,
    /// Actual cumulative time in ms (only with `ANALYZE`).
    pub actual_total_ms: Option<f64>,
    /// Number of loop iterations (only with `ANALYZE`).
    pub actual_loops: Option<f64>,
    /// Children, drawn beneath this node in the tree.
    pub children: Vec<Self>,
}

impl ExplainNode {
    /// Compact one-line summary suited to the tree row label.
    /// Example: `"Seq Scan on users (cost=0.00..21.50 rows=200)"`.
    #[must_use]
    pub fn label(&self) -> String {
        let mut s = self.node_type.clone();
        if let Some(rel) = &self.relation {
            s.push_str(" on ");
            s.push_str(rel);
        } else if let Some(idx) = &self.index {
            s.push_str(" using ");
            s.push_str(idx);
        }
        use std::fmt::Write as _;
        let _ = write!(
            &mut s,
            " (cost={:.2}..{:.2} rows={:.0})",
            self.startup_cost, self.total_cost, self.plan_rows
        );
        s
    }

    /// Recursive max `total_cost` across the subtree. Used by the
    /// renderer to normalise cost bars across the whole plan.
    #[must_use]
    pub fn max_cost(&self) -> f64 {
        let mut m = self.total_cost;
        for c in &self.children {
            let cm = c.max_cost();
            if cm > m {
                m = cm;
            }
        }
        m
    }

    /// `true` when this node's actual rows differ from the planner's
    /// estimate by more than 10× in either direction. Identifies the
    /// nodes most likely to be hurting the plan (bad statistics, stale
    /// `pg_class.reltuples`, missing index).
    ///
    /// Returns `false` if `ANALYZE` data is missing or the planner
    /// rows are 0 (we cannot ratio safely).
    #[must_use]
    pub fn rows_divergent(&self) -> bool {
        let Some(actual) = self.actual_rows else {
            return false;
        };
        if self.plan_rows < 1.0 || actual < 0.0 {
            return false;
        }
        let ratio = actual.max(1.0) / self.plan_rows.max(1.0);
        !(0.1..=10.0).contains(&ratio)
    }
}

/// Parsed plan ready for rendering.
#[derive(Debug, Clone, Default)]
pub struct ExplainPlan {
    pub lines: Vec<ExplainLine>,
    /// v1.1 #3: structured tree mirroring `lines`. `None` when the
    /// plan source isn't a parsable PG JSON document (we fall back
    /// to the text-only renderer).
    pub root: Option<ExplainNode>,
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
    let root = build_node(root_plan);
    Ok(ExplainPlan {
        lines,
        root: Some(root),
        planning_time_ms: entry.get("Planning Time").and_then(Json::as_f64),
        execution_time_ms: entry.get("Execution Time").and_then(Json::as_f64),
    })
}

/// Recursively turn a JSON plan node into an [`ExplainNode`].
fn build_node(node: &Json) -> ExplainNode {
    let node_type = node
        .get("Node Type")
        .and_then(Json::as_str)
        .unwrap_or("Unknown")
        .to_owned();
    let relation = node
        .get("Relation Name")
        .and_then(Json::as_str)
        .map(str::to_owned);
    let index = node
        .get("Index Name")
        .and_then(Json::as_str)
        .map(str::to_owned);
    let filter = node
        .get("Filter")
        .and_then(Json::as_str)
        .map(str::to_owned);
    let startup_cost = node.get("Startup Cost").and_then(Json::as_f64).unwrap_or(0.0);
    let total_cost = node.get("Total Cost").and_then(Json::as_f64).unwrap_or(0.0);
    let plan_rows = node.get("Plan Rows").and_then(Json::as_f64).unwrap_or(0.0);
    let actual_rows = node.get("Actual Rows").and_then(Json::as_f64);
    let actual_total_ms = node.get("Actual Total Time").and_then(Json::as_f64);
    let actual_loops = node.get("Actual Loops").and_then(Json::as_f64);
    let children = node
        .get("Plans")
        .and_then(Json::as_array)
        .map(|arr| arr.iter().map(build_node).collect())
        .unwrap_or_default();
    ExplainNode {
        node_type,
        relation,
        index,
        filter,
        total_cost,
        startup_cost,
        plan_rows,
        actual_rows,
        actual_total_ms,
        actual_loops,
        children,
    }
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
    fn structured_tree_matches_sample() {
        let plan = parse(SAMPLE).unwrap();
        let root = plan.root.expect("tree present");
        assert_eq!(root.node_type, "Limit");
        assert_eq!(root.children.len(), 1);
        let child = &root.children[0];
        assert_eq!(child.node_type, "Seq Scan");
        assert_eq!(child.relation.as_deref(), Some("events"));
        assert!(child.label().contains("Seq Scan on events"));
        assert!((root.max_cost() - 3.40).abs() < 0.001);
    }

    #[test]
    fn rows_divergent_detects_10x_gap() {
        let mut node = ExplainNode {
            plan_rows: 10.0,
            actual_rows: Some(150.0),
            ..Default::default()
        };
        assert!(node.rows_divergent(), "15x over should trigger");
        node.plan_rows = 100.0;
        node.actual_rows = Some(1.0);
        assert!(node.rows_divergent(), "100x under should trigger");
        node.plan_rows = 10.0;
        node.actual_rows = Some(20.0);
        assert!(!node.rows_divergent(), "2x should not trigger");
        node.actual_rows = None;
        assert!(!node.rows_divergent(), "missing actual is silent");
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
