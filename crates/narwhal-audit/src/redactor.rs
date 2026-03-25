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
    /// Lower-cased column / parameter names whose values should be
    /// replaced with `***`.
    pub redact_columns: Vec<String>,
}

/// Stateless masker — cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    cfg: RedactorConfig,
}

impl Redactor {
    /// Build a redactor from its config. Lower-cases all column rules
    /// once so per-event work stays O(rules * fields).
    #[must_use]
    pub fn new(mut cfg: RedactorConfig) -> Self {
        for c in &mut cfg.redact_columns {
            *c = c.to_lowercase();
        }
        Self { cfg }
    }

    /// Apply masking to `event` in place.
    pub fn apply(&self, event: &mut AuditEvent) {
        match event {
            AuditEvent::Query {
                sql, params, error, ..
            } => {
                if self.cfg.redact_passwords {
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
                if !self.cfg.redact_columns.is_empty() {
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
        let lower = name.trim().to_lowercase();
        self.cfg.redact_columns.iter().any(|c| c == &lower)
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
}
