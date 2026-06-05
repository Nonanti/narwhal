//! Audit configuration types.
//!
//! Parsed from the `[settings.audit]` block of `settings.toml`
//! (`schema_version` = 2). Held by the runtime and passed to
//! [`crate::sinks`] to construct the active sink set.

use serde::{Deserialize, Serialize};

/// Top-level audit configuration block.
///
/// Disabled by default — explicitly opt-in to emit any audit data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuditConfig {
    /// Master switch. When false, no sinks are constructed and emit
    /// sites pay only a single atomic load.
    #[serde(default)]
    pub enabled: bool,

    /// One or more sink specifications. Order is preserved; each sink
    /// receives every event.
    #[serde(default)]
    pub sinks: Vec<SinkSpec>,

    /// Whether to redact password / secret literals from the SQL text
    /// of `Query` events. Default true.
    #[serde(default = "default_true")]
    pub redact_passwords: bool,

    /// Additional column names whose values must be masked. Matched
    /// case-insensitively against literal column identifiers and
    /// against bind parameter labels.
    #[serde(default)]
    pub redact_columns: Vec<String>,

    /// When true, query dispatch blocks until the audit channel has
    /// room for the event. Use in compliance-first deployments where
    /// dropping a line is unacceptable. Default false (lossy).
    #[serde(default)]
    pub block_on_full: bool,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sinks: Vec::new(),
            redact_passwords: true,
            redact_columns: Vec::new(),
            block_on_full: false,
        }
    }
}

const fn default_true() -> bool {
    true
}

/// One configured sink. Parsed from a string of the form
/// `kind:argument`, e.g. `file:/var/log/narwhal/audit.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "String", into = "String")]
pub enum SinkSpec {
    /// Append to the given filesystem path. Path may contain strftime
    /// tokens (`%Y`, `%m`, `%d`, etc.) which are expanded at open time.
    File(String),
    /// Write each line to stdout. Useful for ephemeral debug runs.
    Stdout,
    /// Forward to the local syslog daemon (Linux). Requires the
    /// `syslog` cargo feature on `narwhal-audit`.
    Syslog,
}

impl TryFrom<String> for SinkSpec {
    type Error = SinkSpecParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value == "stdout" {
            return Ok(Self::Stdout);
        }
        if value == "syslog" {
            return Ok(Self::Syslog);
        }
        if let Some(rest) = value.strip_prefix("file:") {
            if rest.is_empty() {
                return Err(SinkSpecParseError::EmptyFilePath);
            }
            return Ok(Self::File(rest.to_owned()));
        }
        Err(SinkSpecParseError::UnknownScheme(value))
    }
}

impl From<SinkSpec> for String {
    fn from(spec: SinkSpec) -> Self {
        match spec {
            SinkSpec::File(p) => format!("file:{p}"),
            SinkSpec::Stdout => "stdout".to_owned(),
            SinkSpec::Syslog => "syslog".to_owned(),
        }
    }
}

/// Parse error for [`SinkSpec`] strings.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SinkSpecParseError {
    /// `file:` prefix with no path.
    #[error("sink spec `file:` requires a path argument")]
    EmptyFilePath,
    /// Unrecognised scheme — known: `file:<path>`, `stdout`, `syslog`.
    #[error("unknown sink spec `{0}` (expected `file:<path>`, `stdout`, or `syslog`)")]
    UnknownScheme(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stdout() {
        assert_eq!(
            SinkSpec::try_from("stdout".to_string()).unwrap(),
            SinkSpec::Stdout
        );
    }

    #[test]
    fn parse_syslog() {
        assert_eq!(
            SinkSpec::try_from("syslog".to_string()).unwrap(),
            SinkSpec::Syslog
        );
    }

    #[test]
    fn parse_file() {
        assert_eq!(
            SinkSpec::try_from("file:/var/log/narwhal/audit.jsonl".to_string()).unwrap(),
            SinkSpec::File("/var/log/narwhal/audit.jsonl".into())
        );
    }

    #[test]
    fn parse_file_empty_rejected() {
        assert_eq!(
            SinkSpec::try_from("file:".to_string()).unwrap_err(),
            SinkSpecParseError::EmptyFilePath
        );
    }

    #[test]
    fn parse_unknown_rejected() {
        match SinkSpec::try_from("kafka:foo".to_string()).unwrap_err() {
            SinkSpecParseError::UnknownScheme(s) => assert_eq!(s, "kafka:foo"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn round_trip_through_toml() {
        let cfg = AuditConfig {
            enabled: true,
            sinks: vec![SinkSpec::File("/tmp/audit.jsonl".into()), SinkSpec::Stdout],
            redact_passwords: true,
            redact_columns: vec!["ssn".into()],
            block_on_full: false,
        };
        let s = toml::to_string(&cfg).unwrap();
        let back: AuditConfig = toml::from_str(&s).unwrap();
        assert_eq!(cfg, back);
    }
}
