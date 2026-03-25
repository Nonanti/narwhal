//! Audit event schema.
//!
//! Stable wire format: every variant serialises with `kind` as the
//! discriminant and `snake_case` field names. Adding a new variant is
//! non-breaking; renaming or removing a field is breaking and must
//! bump the `schema_version` documented in `docs/audit.md`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Schema version of the audit JSONL wire format.
///
/// Emitted as the `schema_version` field on every line. Consumers (SIEM
/// ingestion, log shippers) can branch on this to handle future format
/// migrations without ambiguity.
pub const AUDIT_SCHEMA_VERSION: u32 = 1;

/// One audit log line.
///
/// `kind` is the tag. The remaining fields vary per variant. `time` and
/// `schema_version` are part of the envelope, applied by the sink when
/// the line is rendered — not by callers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditEvent {
    /// A new connection / session opened.
    ConnectionOpened {
        /// Logical connection name (as declared in `connections.toml`).
        conn: String,
        /// Authenticated database user, if known.
        user: Option<String>,
        /// Host or socket the connection terminates at.
        host: String,
        /// Session correlation id — every subsequent `Query` /
        /// `ConnectionClosed` event for this connection carries the
        /// same id.
        session_id: Uuid,
    },
    /// A previously-opened session was closed (clean or error).
    ConnectionClosed {
        /// Session correlation id matching the earlier `ConnectionOpened`.
        session_id: Uuid,
        /// Wall-clock duration of the session.
        duration_ms: u64,
    },
    /// A SQL statement executed (or attempted to execute).
    Query {
        /// Session correlation id.
        session_id: Uuid,
        /// The SQL text, post-redaction.
        sql: String,
        /// Bound parameter values, post-redaction. Stringified to avoid
        /// dragging driver-specific value types into the wire format.
        params: Vec<String>,
        /// Row count returned (SELECT) or affected (DML). `None` for
        /// DDL or statements that don't expose a count.
        rows: Option<u64>,
        /// Wall-clock execution time on the driver round-trip.
        elapsed_ms: u64,
        /// True when the statement returned without raising an error.
        succeeded: bool,
        /// Error message when `succeeded == false`, post-redaction.
        error: Option<String>,
    },
    /// A user-facing configuration change (settings file edited, vault
    /// rotated, keybinding updated, etc.).
    Configuration {
        /// Human-readable description of the change.
        change: String,
        /// Actor who initiated the change (CLI user, plugin name, etc.).
        by: String,
    },
    /// A plugin was loaded into the runtime.
    PluginLoaded {
        /// Plugin identifier (`name@version` is conventional).
        plugin: String,
        /// Plugin version string.
        version: String,
        /// Capabilities the plugin requested at load time. Inspect this
        /// when investigating sandbox escapes or surprise behaviour.
        capabilities: Vec<String>,
    },
}

/// Sink-applied envelope around an [`AuditEvent`].
///
/// The sink wraps the event with `time` and `schema_version` before
/// serialising. Constructed by [`render_line`]; callers don't touch it.
#[derive(Debug, Clone, Serialize)]
struct AuditLine<'a> {
    schema_version: u32,
    time: DateTime<Utc>,
    #[serde(flatten)]
    event: &'a AuditEvent,
}

/// Render one event as the canonical JSON line written to a sink.
///
/// Wraps the event with `schema_version` and `time` in the documented
/// envelope shape. The returned `String` does **not** include a trailing
/// newline — the sink appends it.
///
/// # Errors
///
/// Returns [`serde_json::Error`] if the event contains data that fails
/// to serialise. In practice this is never observed at runtime because
/// every variant is composed of trivially-serialisable types; a failure
/// here points at a future variant added without test coverage.
pub fn render_line(event: &AuditEvent, time: DateTime<Utc>) -> Result<String, serde_json::Error> {
    let line = AuditLine {
        schema_version: AUDIT_SCHEMA_VERSION,
        time,
        event,
    };
    serde_json::to_string(&line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_wire_shape() {
        let evt = AuditEvent::Configuration {
            change: "vault rotated".into(),
            by: "cli".into(),
        };
        let time = DateTime::parse_from_rfc3339("2026-06-04T10:00:00.123Z")
            .unwrap()
            .with_timezone(&Utc);
        let json = render_line(&evt, time).unwrap();
        assert!(json.contains(r#""schema_version":1"#));
        assert!(json.contains(r#""kind":"configuration""#));
        assert!(json.contains(r#""change":"vault rotated""#));
        assert!(json.contains(r#""by":"cli""#));
        assert!(json.contains(r#""time":"2026-06-04T10:00:00.123Z""#));
        // No trailing newline; sink owns line termination.
        assert!(!json.ends_with('\n'));
    }

    #[test]
    fn connection_opened_round_trip() {
        let id = Uuid::new_v4();
        let evt = AuditEvent::ConnectionOpened {
            conn: "prod-readonly".into(),
            user: Some("audit_ro".into()),
            host: "db.example.com:5432".into(),
            session_id: id,
        };
        let raw = serde_json::to_string(&evt).unwrap();
        let back: AuditEvent = serde_json::from_str(&raw).unwrap();
        assert_eq!(evt, back);
    }
}
