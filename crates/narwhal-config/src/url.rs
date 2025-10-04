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
//! - `sqlite:<path>` or `sqlite:///<abs-path>`
//!
//! Query-string parameters become entries in
//! [`ConnectionParams::options`].

use std::collections::BTreeMap;
use std::fmt;

use narwhal_core::{ConnectionConfig, ConnectionParams};
use uuid::Uuid;

/// Outcome of [`parse`].
#[derive(Debug, Clone)]
pub struct ParsedUrl {
    pub config: ConnectionConfig,
    pub password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlError {
    MissingScheme,
    UnsupportedScheme(String),
    InvalidPort(String),
    InvalidPercentEscape(String),
    MissingHost,
    MissingDatabase,
    EmptyPath,
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingScheme => f.write_str("connection url missing scheme"),
            Self::UnsupportedScheme(s) => write!(f, "unsupported scheme: {s}"),
            Self::InvalidPort(s) => write!(f, "invalid port: {s}"),
            Self::InvalidPercentEscape(s) => write!(f, "invalid percent escape: {s}"),
            Self::MissingHost => f.write_str("connection url missing host"),
            Self::MissingDatabase => f.write_str("connection url missing database"),
            Self::EmptyPath => f.write_str("connection url missing path"),
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

    let (host, port) = match hostport.rsplit_once(':') {
        // Treat as host:port only if the tail parses as a u16 — IPv6 hosts
        // with port-less forms like `[::1]` should not be misread.
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => {
            let port = p
                .parse::<u16>()
                .map_err(|_| UrlError::InvalidPort(p.to_owned()))?;
            (h.to_owned(), Some(port))
        }
        _ => (hostport.to_owned(), None),
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

    let port_segment = port.map(|p| format!(":{p}")).unwrap_or_default();
    let name = format!("{host}{port_segment}/{database}");

    Ok(ParsedUrl {
        config: ConnectionConfig {
            id: Uuid::new_v4(),
            name,
            driver: driver.to_owned(),
            params: ConnectionParams {
                host: Some(host),
                port,
                database: Some(database),
                username,
                path: None,
                options,
            },
        },
        password,
    })
}

fn parse_sqlite(rest: &str) -> ParsedUrl {
    // sqlite://path  -> "path"
    // sqlite:///abs  -> "/abs"
    // sqlite:./rel   -> "./rel" (caller strips the `sqlite:` prefix)
    let path = rest.to_owned();
    let display = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    ParsedUrl {
        config: ConnectionConfig {
            id: Uuid::new_v4(),
            name: display,
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(path),
                ..Default::default()
            },
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
        out.insert(percent_decode(k)?, percent_decode(v)?);
    }
    Ok(out)
}

fn percent_decode(s: &str) -> Result<String, UrlError> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
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

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            parsed
                .config
                .params
                .options
                .get("sslmode")
                .map(String::as_str),
            Some("require")
        );
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
}
