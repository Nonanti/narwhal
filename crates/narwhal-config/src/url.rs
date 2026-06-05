//! Connection-string (URL) parser.
//!
//! Accepts libpq-style URIs and produces a [`ConnectionConfig`] together
//! with an optional password extracted from the userinfo component.
//!
//! Supported schemes:
//!
//! - `postgres://[user[:password]@]host[:port]/database[?key=value&...]`
//! - `postgresql://...` (alias)
//! - `mysql://[user[:password]@]host[:port]/database[?key=value&...]`
//! - `mariadb://...` (alias)
//! - `mssql://[user[:password]@]host[:port]/database[?key=value&...]`
//! - `sqlserver://...` (alias)
//! - `sqlite:<path>` or `sqlite:///<abs-path>`
//!
//! Query-string parameters become entries in
//! [`ConnectionParams::options`].

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use narwhal_core::{ConnectionConfig, ConnectionParams, SslMode};
use uuid::Uuid;

/// Outcome of [`parse`].
#[derive(Debug, Clone)]
pub struct ParsedUrl {
    pub config: ConnectionConfig,
    pub password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UrlError {
    MissingScheme,
    UnsupportedScheme(String),
    InvalidPort(String),
    InvalidPercentEscape(String),
    MissingHost,
    InvalidHost(String),
    EmptyQueryKey(String),
    MissingDatabase,
    EmptyPath,
    InvalidSslMode(String),
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingScheme => f.write_str("connection url missing scheme"),
            Self::UnsupportedScheme(s) => write!(f, "unsupported scheme: {s}"),
            Self::InvalidPort(s) => write!(f, "invalid port: {s}"),
            Self::InvalidPercentEscape(s) => write!(f, "invalid percent escape: {s}"),
            Self::MissingHost => f.write_str("connection url missing host"),
            Self::InvalidHost(s) => write!(f, "invalid host: {s}"),
            Self::EmptyQueryKey(s) => write!(f, "empty query key in '{s}'"),
            Self::MissingDatabase => f.write_str("connection url missing database"),
            Self::EmptyPath => f.write_str("connection url missing path"),
            Self::InvalidSslMode(v) => write!(
                f,
                "invalid sslmode: {v} (expected disable|prefer|require|verify-ca|verify-full)"
            ),
        }
    }
}

impl std::error::Error for UrlError {}

pub fn parse(url: &str) -> Result<ParsedUrl, UrlError> {
    let (scheme, rest) = match url.split_once("://") {
        Some(pair) => pair,
        None => {
            // SQLite also accepts `sqlite:<path>` without the double slash.
            if let Some(path) = url.strip_prefix("sqlite:") {
                ("sqlite", path)
            } else {
                return Err(UrlError::MissingScheme);
            }
        }
    };

    match scheme.to_ascii_lowercase().as_str() {
        "postgres" | "postgresql" => parse_server("postgres", rest),
        "mysql" | "mariadb" => parse_server("mysql", rest),
        // T1-T2-A: accept both `mssql://` (libpq-style, matches our other
        // schemes) and `sqlserver://` (matches the JDBC convention SQL
        // Server users are already familiar with).
        "mssql" | "sqlserver" => parse_server("mssql", rest),
        "sqlite" => Ok(parse_sqlite(rest)),
        other => Err(UrlError::UnsupportedScheme(other.to_owned())),
    }
}

fn parse_server(driver: &'static str, rest: &str) -> Result<ParsedUrl, UrlError> {
    let (userinfo, hostpath) = match rest.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, rest),
    };

    let (username, password) = match userinfo {
        None => (None, None),
        Some(s) => match s.split_once(':') {
            Some((u, p)) => (Some(percent_decode(u)?), Some(percent_decode(p)?)),
            None => (Some(percent_decode(s)?), None),
        },
    };

    let (hostport, path_query) = match hostpath.find('/') {
        Some(i) => (&hostpath[..i], &hostpath[i..]),
        None => (hostpath, ""),
    };

    if hostport.is_empty() {
        return Err(UrlError::MissingHost);
    }

    // IPv6 hosts use bracket form (`[::1]` or `[::1]:5432`) so the colon
    // inside the address isn't confused with the port separator (L4).
    let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
        let close = rest
            .find(']')
            .ok_or_else(|| UrlError::InvalidHost(hostport.to_owned()))?;
        let host = rest[..close].to_owned();
        let after = &rest[close + 1..];
        let port = if after.is_empty() {
            None
        } else if let Some(p) = after.strip_prefix(':') {
            Some(
                p.parse::<u16>()
                    .map_err(|_| UrlError::InvalidPort(p.to_owned()))?,
            )
        } else {
            return Err(UrlError::InvalidHost(hostport.to_owned()));
        };
        (host, port)
    } else {
        match hostport.rsplit_once(':') {
            // Treat as host:port only if the tail parses as a u16. IPv4 /
            // hostname path.
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => {
                let port = p
                    .parse::<u16>()
                    .map_err(|_| UrlError::InvalidPort(p.to_owned()))?;
                (h.to_owned(), Some(port))
            }
            _ => (hostport.to_owned(), None),
        }
    };

    let (path, query) = match path_query.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path_query, ""),
    };
    let database = path.trim_start_matches('/').to_owned();
    if database.is_empty() {
        return Err(UrlError::MissingDatabase);
    }

    let options = parse_query(query)?;

    // Extract TLS-specific query params into struct fields instead of
    // leaving them in the generic options map.
    let ExtractedSslParams {
        options,
        ssl_mode,
        ssl_root_cert,
        ssl_cert,
        ssl_key,
    } = extract_ssl_params(options)?;

    // Same for SSH: pull `ssh_*` query params into a typed SshConfig so
    // `:url postgres://h/db?ssh_host=jump&ssh_user=ubuntu` works
    // end-to-end without any wizard interaction.
    let (options, ssh) = extract_ssh_params(options)?;

    let port_segment = port.map(|p| format!(":{p}")).unwrap_or_default();
    let name = format!("{host}{port_segment}/{database}");

    Ok(ParsedUrl {
        config: ConnectionConfig {
            id: Uuid::new_v4(),
            name,
            driver: driver.to_owned(),
            params: ConnectionParams::with(|p| {
                p.host = Some(host);
                p.port = port;
                p.database = Some(database);
                p.username = username;
                p.options = options;
                p.ssl_mode = ssl_mode;
                p.ssl_root_cert = ssl_root_cert;
                p.ssl_cert = ssl_cert;
                p.ssl_key = ssl_key;
                p.ssh = ssh;
            }),
        },
        password,
    })
}

/// Pull the `ssh_host`/`ssh_user`/`ssh_port`/`ssh_key` keys out of
/// the options map and assemble them into an [`SshConfig`]. A bare
/// `ssh_host` is invalid (no default user); the parser errors out the
/// same way the wizard would.
fn extract_ssh_params(
    mut options: BTreeMap<String, String>,
) -> Result<(BTreeMap<String, String>, Option<narwhal_core::SshConfig>), UrlError> {
    let host = options.remove("ssh_host");
    let port = options.remove("ssh_port");
    let user = options.remove("ssh_user");
    let key = options.remove("ssh_key");
    let any = host.is_some() || port.is_some() || user.is_some() || key.is_some();
    if !any {
        return Ok((options, None));
    }
    let Some(host) = host else {
        return Err(UrlError::EmptyQueryKey("ssh_host".into()));
    };
    let Some(user) = user else {
        return Err(UrlError::EmptyQueryKey("ssh_user".into()));
    };
    let port = match port {
        Some(p) => Some(p.parse::<u16>().map_err(|_| UrlError::InvalidPort(p))?),
        None => None,
    };
    let mut ssh = narwhal_core::SshConfig::new(host, user);
    ssh.port = port;
    ssh.key_path = key.map(PathBuf::from);
    Ok((options, Some(ssh)))
}

fn parse_sqlite(rest: &str) -> ParsedUrl {
    // sqlite://path  -> "path"
    // sqlite:///abs  -> "/abs"
    // sqlite:./rel   -> "./rel" (caller strips the `sqlite:` prefix)
    let path = rest.to_owned();
    let display = std::path::Path::new(&path)
        .file_name()
        .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
    ParsedUrl {
        config: ConnectionConfig {
            id: Uuid::new_v4(),
            name: display,
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(path);
            }),
        },
        password: None,
    }
}

fn parse_query(qs: &str) -> Result<BTreeMap<String, String>, UrlError> {
    let mut out = BTreeMap::new();
    if qs.is_empty() {
        return Ok(out);
    }
    for pair in qs.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some(kv) => kv,
            None => (pair, ""),
        };
        let key = percent_decode(k)?;
        if key.is_empty() {
            // L7: silently dropping `?=v` was a footgun — reject it so
            // typos surface instead of disappearing.
            return Err(UrlError::EmptyQueryKey(pair.to_owned()));
        }
        out.insert(key, percent_decode(v)?);
    }
    Ok(out)
}

fn percent_decode(s: &str) -> Result<String, UrlError> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                // Reject truncated escapes outright instead of silently
                // emitting a literal `%` — RFC 3986 considers `%X` or a
                // trailing `%` malformed and surfacing the error catches
                // typo'd URLs at parse time.
                if i + 2 >= bytes.len() {
                    return Err(UrlError::InvalidPercentEscape(s.to_owned()));
                }
                let high = hex_digit(bytes[i + 1])
                    .ok_or_else(|| UrlError::InvalidPercentEscape(s.to_owned()))?;
                let low = hex_digit(bytes[i + 2])
                    .ok_or_else(|| UrlError::InvalidPercentEscape(s.to_owned()))?;
                out.push((high << 4) | low);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| UrlError::InvalidPercentEscape(s.to_owned()))
}

const fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parse a libpq-style sslmode string into [`SslMode`].
fn parse_ssl_mode(value: &str) -> Result<SslMode, UrlError> {
    match value.to_ascii_lowercase().as_str() {
        "disable" => Ok(SslMode::Disable),
        "prefer" => Ok(SslMode::Prefer),
        "require" => Ok(SslMode::Require),
        "verify-ca" => Ok(SslMode::VerifyCa),
        "verify-full" => Ok(SslMode::VerifyFull),
        other => Err(UrlError::InvalidSslMode(other.to_owned())),
    }
}

/// Result of extracting TLS-specific query params from the options map.
struct ExtractedSslParams {
    options: BTreeMap<String, String>,
    ssl_mode: SslMode,
    ssl_root_cert: Option<PathBuf>,
    ssl_cert: Option<PathBuf>,
    ssl_key: Option<PathBuf>,
}

/// Remove TLS-specific keys from the generic options map and return them
/// as typed struct fields.
fn extract_ssl_params(
    mut options: BTreeMap<String, String>,
) -> Result<ExtractedSslParams, UrlError> {
    let ssl_mode = match options.remove("sslmode") {
        Some(v) => parse_ssl_mode(&v)?,
        None => SslMode::Prefer,
    };
    let ssl_root_cert = options.remove("sslrootcert").map(PathBuf::from);
    let ssl_cert = options.remove("sslcert").map(PathBuf::from);
    let ssl_key = options.remove("sslkey").map(PathBuf::from);
    Ok(ExtractedSslParams {
        options,
        ssl_mode,
        ssl_root_cert,
        ssl_cert,
        ssl_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn ipv6_host_with_port_parses() {
        let parsed = parse("postgres://[::1]:5432/db").expect("parse");
        assert_eq!(parsed.config.params.host.as_deref(), Some("::1"));
        assert_eq!(parsed.config.params.port, Some(5432));
    }

    #[test]
    fn ipv6_host_without_port_parses() {
        let parsed = parse("postgres://[2001:db8::1]/db").expect("parse");
        assert_eq!(parsed.config.params.host.as_deref(), Some("2001:db8::1"));
        assert_eq!(parsed.config.params.port, None);
    }

    #[test]
    fn ipv6_host_missing_closing_bracket_errors() {
        let err = parse("postgres://[::1/db").unwrap_err();
        assert!(matches!(err, UrlError::InvalidHost(_)));
    }

    #[test]
    fn postgres_with_password_and_options() {
        let parsed = parse(
            "postgres://alice:s%40fe@db.example.com:5432/inventory?sslmode=require&app=narwhal",
        )
        .unwrap();
        assert_eq!(parsed.config.driver, "postgres");
        assert_eq!(parsed.config.name, "db.example.com:5432/inventory");
        assert_eq!(parsed.config.params.host.as_deref(), Some("db.example.com"));
        assert_eq!(parsed.config.params.port, Some(5432));
        assert_eq!(parsed.config.params.database.as_deref(), Some("inventory"));
        assert_eq!(parsed.config.params.username.as_deref(), Some("alice"));
        assert_eq!(parsed.password.as_deref(), Some("s@fe"));
        // sslmode is now a struct field, not in options
        assert_eq!(parsed.config.params.ssl_mode, SslMode::Require);
        assert!(!parsed.config.params.options.contains_key("sslmode"));
        assert_eq!(
            parsed.config.params.options.get("app").map(String::as_str),
            Some("narwhal")
        );
    }

    #[test]
    fn postgres_without_userinfo_or_port() {
        let parsed = parse("postgresql://localhost/postgres").unwrap();
        assert_eq!(parsed.config.driver, "postgres");
        assert_eq!(parsed.config.params.port, None);
        assert!(parsed.config.params.username.is_none());
        assert!(parsed.password.is_none());
    }

    #[test]
    fn mysql_alias_resolves_to_mysql_driver() {
        let parsed = parse("mariadb://root@127.0.0.1:3306/test").unwrap();
        assert_eq!(parsed.config.driver, "mysql");
        assert_eq!(parsed.config.params.port, Some(3306));
    }

    #[test]
    fn mssql_scheme_resolves_to_mssql_driver() {
        let parsed = parse("mssql://sa:hunter2@db.local:1433/master").unwrap();
        assert_eq!(parsed.config.driver, "mssql");
        assert_eq!(parsed.config.params.host.as_deref(), Some("db.local"));
        assert_eq!(parsed.config.params.port, Some(1433));
        assert_eq!(parsed.config.params.database.as_deref(), Some("master"));
        assert_eq!(parsed.config.params.username.as_deref(), Some("sa"));
        assert_eq!(parsed.password.as_deref(), Some("hunter2"));
    }

    #[test]
    fn sqlserver_alias_resolves_to_mssql_driver() {
        let parsed = parse("sqlserver://sa@db/master").unwrap();
        assert_eq!(parsed.config.driver, "mssql");
    }

    #[test]
    fn sqlite_double_slash_and_bare_forms() {
        let a = parse("sqlite:///tmp/data.db").unwrap();
        assert_eq!(a.config.driver, "sqlite");
        assert_eq!(a.config.params.path.as_deref(), Some("/tmp/data.db"));
        assert_eq!(a.config.name, "data.db");

        let b = parse("sqlite:./relative.db").unwrap();
        assert_eq!(b.config.params.path.as_deref(), Some("./relative.db"));
    }

    #[test]
    fn rejects_unknown_scheme() {
        let err = parse("mongo://localhost/db").unwrap_err();
        assert!(matches!(err, UrlError::UnsupportedScheme(_)));
    }

    #[test]
    fn rejects_missing_database() {
        let err = parse("postgres://localhost").unwrap_err();
        assert_eq!(err, UrlError::MissingDatabase);
    }

    #[test]
    fn rejects_out_of_range_port() {
        let err = parse("postgres://h:99999/db").unwrap_err();
        assert!(matches!(err, UrlError::InvalidPort(_)));
    }

    #[test]
    fn non_digit_after_colon_is_treated_as_part_of_host() {
        // Conservative: only an all-digit tail is recognised as a port. This
        // keeps bracketless IPv6-like strings from being misclassified.
        let parsed = parse("postgres://h:notaport/db").unwrap();
        assert_eq!(parsed.config.params.host.as_deref(), Some("h:notaport"));
        assert_eq!(parsed.config.params.port, None);
    }

    #[test]
    fn percent_decode_handles_plus_and_escapes() {
        assert_eq!(percent_decode("hello+world").unwrap(), "hello world");
        assert_eq!(percent_decode("a%2Fb").unwrap(), "a/b");
        assert!(percent_decode("a%ZZ").is_err());
    }

    #[test]
    fn percent_decode_rejects_truncated_escape() {
        // Trailing `%X` or bare `%` used to slip through and emit a
        // literal `%`. Both should error out now.
        assert!(percent_decode("a%").is_err());
        assert!(percent_decode("a%F").is_err());
    }

    #[test]
    fn url_parses_sslmode_to_struct_field() {
        let parsed = parse("postgres://h/db?sslmode=require").unwrap();
        assert_eq!(parsed.config.params.ssl_mode, SslMode::Require);
        assert!(!parsed.config.params.options.contains_key("sslmode"));
    }

    #[test]
    fn url_parses_all_ssl_modes() {
        for (value, expected) in [
            ("disable", SslMode::Disable),
            ("prefer", SslMode::Prefer),
            ("require", SslMode::Require),
            ("verify-ca", SslMode::VerifyCa),
            ("verify-full", SslMode::VerifyFull),
        ] {
            let url = format!("postgres://h/db?sslmode={value}");
            let parsed = parse(&url).unwrap();
            assert_eq!(parsed.config.params.ssl_mode, expected, "sslmode={value}");
        }
    }

    #[test]
    fn url_parses_sslrootcert_path() {
        let parsed =
            parse("postgres://h/db?sslrootcert=/etc/ssl/ca.pem&sslmode=verify-full").unwrap();
        assert_eq!(
            parsed.config.params.ssl_root_cert,
            Some(PathBuf::from("/etc/ssl/ca.pem"))
        );
        // Should NOT appear in options
        assert!(!parsed.config.params.options.contains_key("sslrootcert"));
    }

    #[test]
    fn url_parses_sslcert_and_sslkey() {
        let parsed = parse("postgres://h/db?sslcert=/c.pem&sslkey=/k.pem").unwrap();
        assert_eq!(parsed.config.params.ssl_cert, Some(PathBuf::from("/c.pem")));
        assert_eq!(parsed.config.params.ssl_key, Some(PathBuf::from("/k.pem")));
        assert!(!parsed.config.params.options.contains_key("sslcert"));
        assert!(!parsed.config.params.options.contains_key("sslkey"));
    }

    #[test]
    fn url_rejects_unknown_sslmode() {
        let err = parse("postgres://h/db?sslmode=magic").unwrap_err();
        assert!(matches!(err, UrlError::InvalidSslMode(_)));
        assert!(err.to_string().contains("magic"));
    }

    #[test]
    fn url_no_sslmode_defaults_to_prefer() {
        let parsed = parse("postgres://h/db?app=test").unwrap();
        assert_eq!(parsed.config.params.ssl_mode, SslMode::Prefer);
    }
}
