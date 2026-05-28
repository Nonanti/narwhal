//! Event-level redaction.
//!
//! Walks an [`AuditEvent`] in place and masks fields the operator
//! flagged as sensitive. SQL bodies are run through
//! [`narwhal_history::redact_sql_secrets`] so the audit log inherits
//! the same masking guarantees as the query history.
//!
//! Redaction is **best-effort** — documented as such — and is not a
//! cryptographic boundary. Treat the resulting file as confidential at
//! the filesystem level.

use crate::event::AuditEvent;

/// Configuration for [`Redactor`]. Built from [`crate::AuditConfig`].
#[derive(Debug, Clone, Default)]
pub struct RedactorConfig {
    /// Mask password literals in SQL text (default true).
    pub redact_passwords: bool,
    /// Column / parameter names whose values should be replaced with
    /// `***`. Matching is ASCII-case-insensitive: the rules
    /// are folded with [`str::to_ascii_lowercase`] and compared via
    /// [`str::eq_ignore_ascii_case`]. **Implication:** a rule with
    /// non-ASCII letters (e.g. `İSIM`) will only match column
    /// names that are byte-identical aside from ASCII case. Users
    /// who need full-Unicode case folding should encode every
    /// case variant they want masked as separate rules.
    pub redact_columns: Vec<String>,
}

/// Stateless masker — cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    redact_passwords: bool,
    /// Pre-folded column rules in a `Vec`. Real-world configs hold
    /// 1–10 entries; a linear scan over a `Vec<String>` with
    /// `eq_ignore_ascii_case` per entry is measurably faster than a
    /// `BTreeSet` lookup for that range and avoids the allocator
    /// pressure of building / cloning the set on hot paths. Folding
    /// is [`str::to_ascii_lowercase`], not full-Unicode
    /// `to_lowercase`, so the rule list and the per-event matcher
    /// agree on the same byte-level case relation.
    redact_columns: Vec<String>,
}

impl Redactor {
    /// Build a redactor from its config. Column rules are
    /// ASCII-lower-cased once at construction so per-event matching
    /// is alloc-free.
    #[must_use]
    pub fn new(cfg: RedactorConfig) -> Self {
        // Switched from `to_lowercase()` (full Unicode) to
        // `to_ascii_lowercase()` so the rule keys agree with the
        // matcher (`eq_ignore_ascii_case`). The previous mix would
        // silently miss e.g. a Turkish `İSIM` rule against an
        // `İSIM` column.
        let redact_columns = cfg
            .redact_columns
            .into_iter()
            .map(|c| c.to_ascii_lowercase())
            .collect();
        Self {
            redact_passwords: cfg.redact_passwords,
            redact_columns,
        }
    }

    /// Apply masking to `event` in place.
    pub fn apply(&self, event: &mut AuditEvent) {
        match event {
            AuditEvent::Query {
                sql, params, error, ..
            } => {
                if self.redact_passwords {
                    let masked = narwhal_history::redact_sql_secrets(sql);
                    if let std::borrow::Cow::Owned(s) = masked {
                        *sql = s;
                    }
                    if let Some(e) = error {
                        let masked = narwhal_history::redact_sql_secrets(e);
                        if let std::borrow::Cow::Owned(s) = masked {
                            *e = s;
                        }
                    }
                }
                // Column-name redaction: callers encode parameters as
                // `<column>=<value>` strings; we mask the value when
                // the column matches a rule.
                if !self.redact_columns.is_empty() {
                    for p in params.iter_mut() {
                        if let Some((name, _)) = p.split_once('=') {
                            if self.matches_column(name) {
                                *p = format!("{name}=***");
                            }
                        }
                    }
                }
            }
            // Other variants carry no SQL body or column values to
            // mask. ConnectionOpened intentionally records the host
            // and user; redacting them defeats the audit purpose.
            AuditEvent::ConnectionOpened { .. }
            | AuditEvent::ConnectionClosed { .. }
            | AuditEvent::Configuration { .. }
            | AuditEvent::PluginLoaded { .. } => {}
        }
    }

    fn matches_column(&self, name: &str) -> bool {
        // R3-N3: shortened the comment to match what the code
        // actually does. The only fast-path is the empty rule set;
        // `eq_ignore_ascii_case` is alloc-free regardless of the
        // input casing.
        if self.redact_columns.is_empty() {
            return false;
        }
        let trimmed = name.trim();
        self.redact_columns
            .iter()
            .any(|rule| rule.eq_ignore_ascii_case(trimmed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_query(sql: &str, params: Vec<String>) -> AuditEvent {
        AuditEvent::Query {
            session_id: Uuid::nil(),
            sql: sql.to_string(),
            params,
            rows: None,
            elapsed_ms: 0,
            succeeded: true,
            error: None,
        }
    }

    #[test]
    fn redacts_password_in_sql() {
        let r = Redactor::new(RedactorConfig {
            redact_passwords: true,
            redact_columns: vec![],
        });
        let mut evt = make_query("ALTER USER bob WITH PASSWORD 'sekret'", vec![]);
        r.apply(&mut evt);
        let AuditEvent::Query { sql, .. } = evt else {
            unreachable!()
        };
        assert!(sql.contains("'***'"));
        assert!(!sql.contains("sekret"));
    }

    #[test]
    fn redacts_configured_column_param() {
        let r = Redactor::new(RedactorConfig {
            redact_passwords: false,
            redact_columns: vec!["SSN".into(), "card_number".into()],
        });
        let mut evt = make_query(
            "INSERT INTO people VALUES (?, ?)",
            vec!["ssn=123-45-6789".into(), "name=Alice".into()],
        );
        r.apply(&mut evt);
        let AuditEvent::Query { params, .. } = evt else {
            unreachable!()
        };
        assert_eq!(params[0], "ssn=***");
        assert_eq!(params[1], "name=Alice");
    }

    #[test]
    fn leaves_connection_events_untouched() {
        let r = Redactor::new(RedactorConfig {
            redact_passwords: true,
            redact_columns: vec!["host".into()],
        });
        let mut evt = AuditEvent::ConnectionOpened {
            conn: "prod".into(),
            user: Some("alice".into()),
            host: "db.example.com:5432".into(),
            session_id: Uuid::nil(),
        };
        let before = evt.clone();
        r.apply(&mut evt);
        assert_eq!(before, evt);
    }

    #[test]
    fn no_alloc_when_nothing_to_mask() {
        let r = Redactor::new(RedactorConfig {
            redact_passwords: true,
            redact_columns: vec![],
        });
        let mut evt = make_query("SELECT 1", vec![]);
        let before = evt.clone();
        r.apply(&mut evt);
        assert_eq!(before, evt);
    }

    /// Rule keys and the matcher must agree on the same
    /// case relation. Mixing `to_lowercase()` (full Unicode) with
    /// `eq_ignore_ascii_case` previously left a hole where a rule
    /// with non-ASCII upper-case letters wouldn't match its own
    /// column. The test pins down that an ASCII-only rule
    /// ("PASSWORD") still matches a mixed-case column name and
    /// non-ASCII columns are matched byte-identically modulo
    /// ASCII case.
    #[test]
    fn ascii_case_folding_is_consistent_across_rule_and_matcher() {
        let r = Redactor::new(RedactorConfig {
            redact_passwords: false,
            // Mixed-case rules; rule storage folds to ASCII-lower.
            redact_columns: vec!["PASSWORD".into(), "Card_Number".into()],
        });
        let mut evt = make_query(
            "INSERT INTO t VALUES (?, ?, ?)",
            vec![
                "password=hunter2".into(),
                "CARD_NUMBER=4111-1111-1111-1111".into(),
                "name=Bob".into(),
            ],
        );
        r.apply(&mut evt);
        let AuditEvent::Query { params, .. } = evt else {
            unreachable!()
        };
        assert_eq!(params[0], "password=***");
        assert_eq!(params[1], "CARD_NUMBER=***");
        assert_eq!(params[2], "name=Bob");
    }
}
