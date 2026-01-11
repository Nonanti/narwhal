//! Environment-variable and `~/.pgpass`-style password resolution.
//!
//! Used as a fallback when the keyring lookup turns up empty so that
//! `narwhal` plays well with the same workflows people already have
//! configured for `psql` / `mysql`.
//!
//! # Lookup order
//!
//! 1. Driver-specific environment variable (`PGPASSWORD`,
//!    `MYSQL_PWD`, `CLICKHOUSE_PASSWORD`).
//! 2. `~/.pgpass` (libpq format) for postgres, `~/.my.cnf`-style
//!    fallbacks are *not* implemented (the file is interactive and
//!    `mysql_config_editor` is a separate beast — users on that
//!    workflow can keep typing the password).
//!
//! # `~/.pgpass` format (postgres)
//!
//! Each non-blank, non-`#` line is `host:port:database:user:password`.
//! `*` matches any value in any of the first four columns. Colons and
//! backslashes inside fields are escaped with a leading backslash.
//! libpq itself refuses to read the file if its mode is broader than
//! `0600`; we do the same on Unix and skip the check on Windows.
//!
//! See: <https://www.postgresql.org/docs/current/libpq-pgpass.html>
//!
//! The fallback is best-effort: any failure to open/parse the file is
//! logged at debug level and treated as "no password" so a broken
//! `~/.pgpass` never blocks the connect path.

use std::path::PathBuf;

use narwhal_core::ConnectionParams;

/// Try the environment variable that the upstream CLI tooling uses
/// for each driver. Returns `None` when the variable is unset or
/// empty so an empty `PGPASSWORD=` doesn't shadow a real keyring
/// entry.
pub fn password_from_env(driver: &str) -> Option<String> {
    let key = match driver {
        "postgres" => "PGPASSWORD",
        "mysql" => "MYSQL_PWD",
        "clickhouse" => "CLICKHOUSE_PASSWORD",
        _ => return None,
    };
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Resolve a password from `~/.pgpass` (postgres only). Honours the
/// `PGPASSFILE` environment variable just like libpq. Returns `None`
/// for anything other than `driver == "postgres"`.
pub fn password_from_pgpass(driver: &str, params: &ConnectionParams) -> Option<String> {
    if driver != "postgres" {
        return None;
    }
    let path = pgpass_path()?;
    if !is_pgpass_mode_safe(&path) {
        tracing::debug!(
            target: "narwhal::pgpass",
            path = %path.display(),
            "skipping ~/.pgpass: mode broader than 0600"
        );
        return None;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!(target: "narwhal::pgpass", error = %e, "could not read pgpass");
            return None;
        }
    };
    let host = params.host.as_deref().unwrap_or("localhost");
    let port = params.port.map(|p| p.to_string()).unwrap_or_default();
    let database = params.database.as_deref().unwrap_or("");
    let username = params.username.as_deref().unwrap_or("");
    match_pgpass(&text, host, &port, database, username)
}

/// Combined entry point: env first, then `~/.pgpass`. Callers feed
/// this only when the keyring returned nothing so the precedence
/// stays `keyring > env > pgpass > prompt`.
pub fn resolve_password(driver: &str, params: &ConnectionParams) -> Option<String> {
    password_from_env(driver).or_else(|| password_from_pgpass(driver, params))
}

fn pgpass_path() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("PGPASSFILE") {
        if !custom.is_empty() {
            return Some(PathBuf::from(custom));
        }
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".pgpass"))
}

#[cfg(unix)]
fn is_pgpass_mode_safe(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    // libpq rejects anything with group/other bits set.
    let mode = meta.permissions().mode() & 0o077;
    mode == 0
}

#[cfg(not(unix))]
fn is_pgpass_mode_safe(_path: &std::path::Path) -> bool {
    // No POSIX permissions to inspect; trust the user.
    true
}

fn match_pgpass(
    text: &str,
    host: &str,
    port: &str,
    database: &str,
    username: &str,
) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(fields) = split_pgpass_line(line) else {
            continue;
        };
        if fields.len() != 5 {
            continue;
        }
        if !matches_field(&fields[0], host)
            || !matches_field(&fields[1], port)
            || !matches_field(&fields[2], database)
            || !matches_field(&fields[3], username)
        {
            continue;
        }
        return Some(fields[4].clone());
    }
    None
}

/// `*` is the libpq wildcard.
fn matches_field(pattern: &str, value: &str) -> bool {
    pattern == "*" || pattern == value
}

/// Split on unescaped `:`. `\:` is a literal colon, `\\` is a literal
/// backslash. Returns `None` for lines containing a dangling backslash.
fn split_pgpass_line(line: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = vec![String::new()];
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(escaped @ (':' | '\\')) => out.last_mut()?.push(escaped),
                Some(other) => {
                    // Unknown escape: preserve verbatim so we don't
                    // silently mangle a password containing `\n`.
                    out.last_mut()?.push('\\');
                    out.last_mut()?.push(other);
                }
                None => return None,
            },
            ':' => out.push(String::new()),
            other => out.last_mut()?.push(other),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(host: &str, port: u16, db: &str, user: &str) -> ConnectionParams {
        ConnectionParams {
            host: Some(host.into()),
            port: Some(port),
            database: Some(db.into()),
            username: Some(user.into()),
            ..Default::default()
        }
    }

    #[test]
    fn match_exact_line() {
        let text = "db.example.com:5432:inventory:alice:s3cret\n";
        let pw = match_pgpass(text, "db.example.com", "5432", "inventory", "alice");
        assert_eq!(pw.as_deref(), Some("s3cret"));
    }

    #[test]
    fn wildcard_host_matches() {
        let text = "*:*:*:alice:fallback\n";
        let pw = match_pgpass(text, "anywhere", "1234", "anydb", "alice");
        assert_eq!(pw.as_deref(), Some("fallback"));
    }

    #[test]
    fn first_match_wins() {
        let text = "*:*:*:alice:first\n*:*:*:alice:second\n";
        let pw = match_pgpass(text, "x", "5432", "y", "alice");
        assert_eq!(pw.as_deref(), Some("first"));
    }

    #[test]
    fn comments_and_blank_lines_skipped() {
        let text = "# leading comment\n\n*:*:*:bob:bobs-pw\n";
        let pw = match_pgpass(text, "h", "5432", "d", "bob");
        assert_eq!(pw.as_deref(), Some("bobs-pw"));
    }

    #[test]
    fn no_user_match_returns_none() {
        let text = "*:*:*:alice:s3cret\n";
        let pw = match_pgpass(text, "h", "5432", "d", "bob");
        assert!(pw.is_none());
    }

    #[test]
    fn escaped_colon_in_password_is_kept_intact() {
        let text = r"h:5432:d:u:pa\:ss";
        let pw = match_pgpass(text, "h", "5432", "d", "u");
        assert_eq!(pw.as_deref(), Some("pa:ss"));
    }

    #[test]
    fn malformed_line_is_skipped_not_errored() {
        let text = "only-three:fields:here\n*:*:*:alice:winning\n";
        let pw = match_pgpass(text, "h", "5432", "d", "alice");
        assert_eq!(pw.as_deref(), Some("winning"));
    }

    #[test]
    fn non_postgres_driver_short_circuits() {
        let p = params("h", 3306, "d", "u");
        assert!(password_from_pgpass("mysql", &p).is_none());
    }

    /// Both env-var assertions live in one test so the surrounding
    /// process-global `PGPASSWORD` set/remove can't race other tests
    /// running in parallel. `std::env::set_var` is process-wide, so
    /// splitting these into two `#[test]` functions made them
    /// flake under cargo's default thread pool.
    #[test]
    fn env_var_resolution_round_trip() {
        std::env::set_var("PGPASSWORD", "from-env");
        let pw = password_from_env("postgres");
        assert_eq!(pw.as_deref(), Some("from-env"));
        assert!(password_from_env("sqlite").is_none());

        std::env::set_var("PGPASSWORD", "");
        assert!(password_from_env("postgres").is_none());
        std::env::remove_var("PGPASSWORD");
        assert!(password_from_env("postgres").is_none());
    }
}
