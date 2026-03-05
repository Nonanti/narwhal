//! Shared test helpers for the `MySQL` driver byte-accuracy tests.

use narwhal_core::{Connection, ConnectionConfig, ConnectionParams, DatabaseDriver};
use narwhal_driver_mysql::MysqlDriver;

/// Connect to a `MySQL` instance specified by the `NARWHAL_MYSQL_URL`
/// environment variable. Returns `Ok(None)` when the variable is unset so
/// callers can skip gracefully.
pub(crate) async fn test_connect() -> narwhal_core::Result<Option<Box<dyn Connection>>> {
    let url = match std::env::var("NARWHAL_MYSQL_URL") {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let config = parse_url(&url)?;
    let conn = MysqlDriver::new().connect(&config, None).await?;
    Ok(Some(conn))
}

/// Minimal URL parser that extracts host, port, dbname, and user from a
/// `mysql://user@host:3306/dbname` style connection string.
fn parse_url(url: &str) -> narwhal_core::Result<ConnectionConfig> {
    let stripped = url.strip_prefix("mysql://").unwrap_or(url);
    // Split into user@host:port/dbname
    let (user_part, rest) = stripped
        .split_once('@')
        .map_or((None, stripped), |(u, r)| (Some(u), r));
    let (host_port, dbname) = rest
        .split_once('/')
        .map_or((rest, None), |(hp, db)| (hp, Some(db)));
    let (host, port) = host_port
        .split_once(':')
        .map_or((host_port, None), |(h, p)| (h, p.parse::<u16>().ok()));
    Ok(ConnectionConfig {
        id: uuid::Uuid::nil(),
        name: "byte_test".into(),
        driver: MysqlDriver::NAME.into(),
        params: ConnectionParams::with(|p| {
            p.host = Some(host.to_owned());
            p.port = port;
            p.database = dbname.map(str::to_owned);
            p.username = user_part.map(str::to_owned);
        }),
    })
}
