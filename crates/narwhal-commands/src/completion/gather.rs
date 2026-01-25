//! Ranking and assembly of completion candidates.

use std::collections::{BTreeSet, HashMap};

use narwhal_core::ColumnHeader;
use narwhal_domain::SchemaListing;

use super::context::{detect_context_with_schemas, CompletionContext};
use super::items::{Completion, CompletionKind};
use super::keywords::{FUNCTIONS, KEYWORDS, PHRASES};

pub fn gather(
    prefix: &str,
    schemas: &[SchemaListing],
    context: &CompletionContext,
    columns: &HashMap<String, (String, Vec<ColumnHeader>)>,
    limit: usize,
) -> Vec<Completion> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let lower_prefix = prefix.to_ascii_lowercase();

    let mut prefix_hits: Vec<Completion> = Vec::new();
    let mut substr_hits: Vec<Completion> = Vec::new();
    let mut seen: BTreeSet<(CompletionKind, String)> = BTreeSet::new();

    let mut push = |c: Completion| {
        let key = (c.kind, c.text.to_ascii_lowercase());
        if seen.contains(&key) {
            return;
        }
        let lower = c.text.to_ascii_lowercase();
        if lower.starts_with(&lower_prefix) {
            seen.insert(key);
            prefix_hits.push(c);
        } else if lower.contains(&lower_prefix) {
            seen.insert(key);
            substr_hits.push(c);
        }
    };

    match context {
        CompletionContext::TableExpected => {
            // Only tables — keywords after FROM/JOIN/etc. are never valid
            // SQL and would dilute the results.
            for (schema, tables) in schemas {
                for table in tables {
                    let detail = if schema.name.is_empty() {
                        None
                    } else {
                        Some(schema.name.clone())
                    };
                    push(Completion {
                        text: table.name.clone(),
                        kind: CompletionKind::Table,
                        detail,
                    });
                }
            }
        }
        CompletionContext::ColumnExpected { table } => {
            let lower_table = table.to_ascii_lowercase();
            if let Some((schema_name, cols)) = columns.get(&lower_table) {
                for col in cols {
                    let detail = if schema_name.is_empty() {
                        None
                    } else {
                        Some(schema_name.clone())
                    };
                    push(Completion {
                        text: col.name.clone(),
                        kind: CompletionKind::Column,
                        detail,
                    });
                }
            }
        }
        CompletionContext::SchemaTableExpected { schema } => {
            // Only emit tables whose owning schema name matches. We
            // skip the schema-name detail because the prefix already
            // displays it visually.
            for (s, tables) in schemas {
                if !s.name.eq_ignore_ascii_case(schema) {
                    continue;
                }
                for table in tables {
                    push(Completion {
                        text: table.name.clone(),
                        kind: CompletionKind::Table,
                        detail: None,
                    });
                }
            }
        }
        CompletionContext::Generic => {
            for keyword in KEYWORDS {
                push(Completion {
                    text: (*keyword).to_owned(),
                    kind: CompletionKind::Keyword,
                    detail: None,
                });
            }
            for phrase in PHRASES {
                push(Completion {
                    text: (*phrase).to_owned(),
                    kind: CompletionKind::Keyword,
                    detail: None,
                });
            }
            for func in FUNCTIONS {
                push(Completion {
                    text: (*func).to_owned(),
                    kind: CompletionKind::Function,
                    detail: None,
                });
            }
            for (schema, tables) in schemas {
                for table in tables {
                    let detail = if schema.name.is_empty() {
                        None
                    } else {
                        Some(schema.name.clone())
                    };
                    push(Completion {
                        text: table.name.clone(),
                        kind: CompletionKind::Table,
                        detail,
                    });
                }
            }
        }
    }

    // Sort each tier alphabetically (case-insensitive) for predictability.
    let cmp = |a: &Completion, b: &Completion| {
        a.text
            .to_ascii_lowercase()
            .cmp(&b.text.to_ascii_lowercase())
    };
    prefix_hits.sort_by(cmp);
    substr_hits.sort_by(cmp);

    let mut out = prefix_hits;
    out.extend(substr_hits);
    out.truncate(limit);
    out
}

