//! Smoke tests for the MSSQL driver.
//!
//! Two execution modes:
//!
//! 1. **Docker-backed integration**: requires Docker. The
//!    `testcontainers` harness pulls
//!    `mcr.microsoft.com/mssql/server:2022-latest`, waits for it to
//!    accept connections, and exercises the full driver path
//!    (connect, DML, schema introspection, transactions, streaming).
//!    Gated behind `#[ignore]` so it doesn't run on the default
//!    `cargo test` invocation. Run locally with:
//!
//!    ```sh
//!    cd crates/narwhal-drivers/tests/fixtures/mssql && docker compose up -d
//!    cargo test -p narwhal-drivers --features mssql -- --ignored
//!    ```
//!
//! 2. **Environment-variable URL**: when `NARWHAL_MSSQL_URL` is set,
//!    [`from_url`] connects to it directly and runs a single
//!    `SELECT 1` smoke check that's safe to run in CI on a shared
//!    instance. Useful for testing against Azure SQL DB or a long-
//!    lived dev container without re-pulling the image.

#![cfg(feature = "mssql")]
#![allow(clippy::missing_panics_doc)]

use std::time::Duration;

use narwhal_core::{
    ConnectionConfig, ConnectionParams, DatabaseDriver, IsolationLevel, TableKind, Value,
};
use narwhal_drivers::mssql::MssqlDriver;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::mssql_server::MssqlServer;
use uuid::Uuid;

/// Docker harness mirroring the postgres `Harness::start` pattern.
///
/// Holds the container handle so it gets reaped at drop time; the test
/// must keep `Harness` alive for the duration of its `connect` call.
struct Harness {
    _container: testcontainers::ContainerAsync<MssqlServer>,
    driver: MssqlDriver,
    config: ConnectionConfig,
    password: String,
}

impl Harness {
    async fn start() -> Self {
        // EULA must be accepted explicitly per Microsoft licence; the
        // testcontainers wrapper exposes a builder for it.
        let container = MssqlServer::default()
            .with_accept_eula()
            .start()
            .await
            .expect("start mssql container");
        let port = container
            .get_host_port_ipv4(1433)
            .await
            .expect("mssql host port");
        let host = container.get_host().await.expect("mssql host").to_string();

        let mut options = std::collections::BTreeMap::new();
        // The official image ships a self-signed certificate, so
        // trust it explicitly. Unsafe for production use.
        options.insert("trust_server_certificate".into(), "true".into());

        let config = ConnectionConfig {
            id: Uuid::nil(),
            name: "it".into(),
            driver: MssqlDriver::NAME.into(),
            params: ConnectionParams::with(|p| {
                p.host = Some(host);
                p.port = Some(port);
                p.database = Some("master".into());
                p.username = Some("sa".into());
                p.options = options;
            }),
        };

        Self {
            _container: container,
            driver: MssqlDriver::new(),
            config,
            password: MssqlServer::DEFAULT_SA_PASSWORD.to_owned(),
        }
    }

    async fn connect(&self) -> Box<dyn narwhal_core::DynConnection> {
        // The image accepts TCP almost immediately but spends a few
        // seconds finishing first-time setup; testcontainer's ready
        // signal occasionally fires before SQL Server accepts logins.
        // 8×500ms = ~4s upper bound, well below the per-test budget.
        let mut last = None;
        for _ in 0..8 {
            match self
                .driver
                .connect(&self.config, Some(&self.password))
                .await
            {
                Ok(conn) => return conn,
                Err(e) => {
                    last = Some(e);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
        panic!("driver connect: {last:?}");
    }
}

#[tokio::test]
#[ignore = "requires docker"]
async fn round_trip_select_and_parameter_binding() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute(
        "CREATE TABLE items (id INT IDENTITY(1,1) PRIMARY KEY, \
         name NVARCHAR(64) NOT NULL, qty INT NULL)",
        &[],
    )
    .await
    .unwrap();

    let insert = conn
        .execute(
            "INSERT INTO items (name, qty) VALUES (@P1, @P2)",
            &[Value::String("widget".into()), Value::Int(7)],
        )
        .await
        .unwrap();
    assert_eq!(insert.rows_affected, Some(1));

    let select = conn
        .execute(
            "SELECT name, qty FROM items WHERE qty >= @P1",
            &[Value::Int(1)],
        )
        .await
        .unwrap();
    assert_eq!(select.rows.len(), 1);
    assert_eq!(
        select.rows[0].get(0).map(Value::render),
        Some("widget".into())
    );
    assert_eq!(select.rows[0].get(1).map(Value::render), Some("7".into()));
}

#[tokio::test]
#[ignore = "requires docker"]
async fn transaction_rollback_discards_changes() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute(
        "CREATE TABLE counters (k NVARCHAR(32) PRIMARY KEY, v INT)",
        &[],
    )
    .await
    .unwrap();

    conn.begin_with(IsolationLevel::ReadCommitted)
        .await
        .unwrap();
    conn.execute(
        "INSERT INTO counters VALUES (@P1, @P2)",
        &[Value::String("a".into()), Value::Int(1)],
    )
    .await
    .unwrap();
    conn.rollback().await.unwrap();

    let select = conn
        .execute("SELECT COUNT(*) FROM counters", &[])
        .await
        .unwrap();
    assert_eq!(select.rows[0].get(0).map(Value::render), Some("0".into()));
}

#[tokio::test]
#[ignore = "requires docker"]
async fn savepoint_partial_rollback() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute("CREATE TABLE t (n INT)", &[]).await.unwrap();
    conn.begin().await.unwrap();
    conn.execute("INSERT INTO t VALUES (1)", &[]).await.unwrap();
    conn.savepoint("sp1").await.unwrap();
    conn.execute("INSERT INTO t VALUES (2)", &[]).await.unwrap();
    conn.rollback_to_savepoint("sp1").await.unwrap();
    // Release is a no-op on MSSQL (see driver doc).
    conn.release_savepoint("sp1").await.unwrap();
    conn.commit().await.unwrap();

    let result = conn
        .execute("SELECT n FROM t ORDER BY n", &[])
        .await
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].get(0).map(Value::render), Some("1".into()));
}

#[tokio::test]
#[ignore = "requires docker"]
async fn streaming_consumes_rows() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute("CREATE TABLE nums (n INT)", &[])
        .await
        .unwrap();
    // Generate 100 rows. MSSQL has no `generate_series`; use a recursive
    // CTE which is the idiomatic substitute.
    conn.execute(
        "WITH n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x < 100) \
         INSERT INTO nums (n) SELECT x FROM n OPTION (MAXRECURSION 200)",
        &[],
    )
    .await
    .unwrap();

    let mut stream = conn
        .stream("SELECT n FROM nums ORDER BY n", &[])
        .await
        .unwrap();
    assert_eq!(stream.columns().len(), 1);

    let mut total: i64 = 0;
    let mut count: i64 = 0;
    while let Some(row) = stream.next_row().await.unwrap() {
        if let Some(Value::Int(n)) = row.get(0) {
            total += *n;
            count += 1;
        }
    }
    assert_eq!(count, 100);
    assert_eq!(total, (1..=100).sum::<i64>());
}

#[tokio::test]
#[ignore = "requires docker"]
async fn schema_introspection_round_trip() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    // Use the default `dbo` schema; creating a fresh one would need
    // GRANT and gives nothing for the test.
    conn.execute(
        "CREATE TABLE dbo.customers (\
            id INT IDENTITY(1,1) PRIMARY KEY,\
            email NVARCHAR(255) NOT NULL UNIQUE,\
            created_at DATETIME2 NOT NULL DEFAULT SYSUTCDATETIME()\
         )",
        &[],
    )
    .await
    .unwrap();

    conn.execute(
        "CREATE TABLE dbo.orders (\
            id INT IDENTITY(1,1) PRIMARY KEY,\
            customer_id INT NOT NULL,\
            CONSTRAINT fk_orders_customer FOREIGN KEY (customer_id) \
                REFERENCES dbo.customers(id) ON DELETE CASCADE\
         )",
        &[],
    )
    .await
    .unwrap();

    let schemas = conn.list_schemas().await.unwrap();
    assert!(schemas.iter().any(|s| s.name == "dbo"));

    let tables = conn.list_tables("dbo").await.unwrap();
    let names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();
    assert!(
        names.contains(&"customers"),
        "missing customers in {names:?}"
    );
    assert!(names.contains(&"orders"), "missing orders in {names:?}");
    // The kinds we created are base tables; system views shipped with
    // the dbo schema may also appear, so check just the rows we own.
    for owned in ["customers", "orders"] {
        let entry = tables.iter().find(|t| t.name == owned).expect(owned);
        assert!(
            matches!(entry.kind, TableKind::Table),
            "expected {owned} to be a TableKind::Table, got {:?}",
            entry.kind
        );
    }

    let described = conn.describe_table("dbo", "orders").await.unwrap();
    assert_eq!(described.columns.len(), 2);
    assert!(described.columns.iter().any(|c| c.primary_key));
    assert_eq!(described.foreign_keys.len(), 1);
    let fk = &described.foreign_keys[0];
    assert_eq!(fk.referenced_table, "customers");
    assert_eq!(fk.columns, vec!["customer_id".to_owned()]);
    assert_eq!(fk.referenced_columns, vec!["id".to_owned()]);

    let customers = conn.describe_table("dbo", "customers").await.unwrap();
    assert!(
        !customers.unique_constraints.is_empty(),
        "expected the email UNIQUE constraint to surface, got {:?}",
        customers.unique_constraints
    );

    // DDL round-trip.
    let ddl = conn.fetch_ddl("dbo", "customers").await.unwrap();
    assert!(ddl.contains("CREATE TABLE [dbo].[customers]"), "{ddl}");
    assert!(ddl.contains("IDENTITY"), "{ddl}");
    assert!(ddl.contains("PRIMARY KEY"), "{ddl}");
}

#[tokio::test]
#[ignore = "requires docker"]
async fn ping_and_close() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;
    conn.ping().await.unwrap();
    conn.close().await.unwrap();
}

/// C2 regression: `set_read_only(true)` must actually prevent writes.
/// The pre-fix implementation issued `SET TRANSACTION ISOLATION LEVEL
/// SNAPSHOT` and returned `Ok(())` while every INSERT still committed.
/// The new behaviour refuses mutating SQL at the driver layer with
/// `Error::Unsupported`.
#[tokio::test]
#[ignore = "requires docker"]
async fn read_only_refuses_mutating_sql_at_driver_layer() {
    use narwhal_core::Error;

    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute("CREATE TABLE dbo.ro_t (id INT PRIMARY KEY)", &[])
        .await
        .unwrap();

    // Reads stay open.
    conn.set_read_only(true).await.unwrap();
    conn.execute("SELECT COUNT(*) FROM dbo.ro_t", &[])
        .await
        .unwrap();

    // Writes blocked at the driver — the wire never sees the SQL.
    let err = conn
        .execute("INSERT INTO dbo.ro_t VALUES (1)", &[])
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Unsupported(_)),
        "expected Error::Unsupported, got {err:?}"
    );

    // The same applies to OUTPUT clauses and CTE-driven DML.
    let err = conn
        .execute("INSERT INTO dbo.ro_t OUTPUT inserted.id VALUES (2)", &[])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));

    // Table stays empty — confirm the refusals were real.
    let count = conn
        .execute("SELECT COUNT(*) FROM dbo.ro_t", &[])
        .await
        .unwrap();
    assert_eq!(
        count.rows[0].get(0).map(Value::render),
        Some("0".into()),
        "read-only enforcement was bypassed"
    );

    // Re-enable writes and verify the flag releases cleanly.
    conn.set_read_only(false).await.unwrap();
    conn.execute("INSERT INTO dbo.ro_t VALUES (3)", &[])
        .await
        .unwrap();
    let count = conn
        .execute("SELECT COUNT(*) FROM dbo.ro_t", &[])
        .await
        .unwrap();
    assert_eq!(count.rows[0].get(0).map(Value::render), Some("1".into()));
}

/// C1 regression: an INSERT with an OUTPUT clause must yield the
/// returned rows. The pre-fix implementation routed any statement
/// starting with INSERT through `Client::execute`, which discarded
/// rows; this test would have surfaced `result.rows.is_empty()` even
/// though the SQL produced one row.
#[tokio::test]
#[ignore = "requires docker"]
async fn insert_output_clause_preserves_returned_rows() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute(
        "CREATE TABLE dbo.out_t (\
           id INT IDENTITY(1,1) PRIMARY KEY, \
           tag NVARCHAR(32) NOT NULL)",
        &[],
    )
    .await
    .unwrap();

    let result = conn
        .execute(
            "INSERT INTO dbo.out_t (tag) OUTPUT inserted.id, inserted.tag \
             VALUES (@P1), (@P2)",
            &[Value::String("a".into()), Value::String("b".into())],
        )
        .await
        .unwrap();

    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows_affected, Some(2));
    let tags: Vec<String> = result
        .rows
        .iter()
        .filter_map(|r| match r.get(1) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(tags.contains(&"a".to_owned()));
    assert!(tags.contains(&"b".to_owned()));
}

/// C1 regression: EXEC of a stored procedure that returns a row set
/// must hand back those rows. The pre-fix path classified `EXEC` as
/// mutating and ran it through `Client::execute`, which discards
/// rows; the user saw an empty result.
#[tokio::test]
#[ignore = "requires docker"]
async fn exec_stored_procedure_preserves_returned_rows() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    // `sp_who` is a built-in system proc that returns a row per
    // active session. The exact row count is non-deterministic but
    // "at least one" is reliable on any healthy instance.
    let result = conn.execute("EXEC sp_who", &[]).await.unwrap();
    assert!(
        !result.rows.is_empty(),
        "EXEC sp_who returned no rows; rows must not be discarded"
    );
    // EXEC rows-affected is intentionally None — see
    // StatementShape::MutatingWithRows docs.
    assert_eq!(result.rows_affected, None);
}

/// C1 regression: a CTE that ultimately drives an INSERT must keep
/// any rows produced by an OUTPUT clause and avoid a routing slip
/// where `rows_affected` silently goes missing.
#[tokio::test]
#[ignore = "requires docker"]
async fn cte_driven_dml_routes_through_query_path() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute("CREATE TABLE dbo.cte_src (id INT, tag NVARCHAR(32))", &[])
        .await
        .unwrap();
    conn.execute("CREATE TABLE dbo.cte_dst (id INT, tag NVARCHAR(32))", &[])
        .await
        .unwrap();
    conn.execute(
        "INSERT INTO dbo.cte_src VALUES (1, 'alpha'), (2, 'beta')",
        &[],
    )
    .await
    .unwrap();

    // CTE feeding a plain INSERT — no OUTPUT, rows_affected = None
    // because tiberius can't surface it via the query path.
    conn.execute(
        "WITH src AS (SELECT id, tag FROM dbo.cte_src) \
         INSERT INTO dbo.cte_dst (id, tag) SELECT id, tag FROM src",
        &[],
    )
    .await
    .unwrap();

    let count = conn
        .execute("SELECT COUNT(*) FROM dbo.cte_dst", &[])
        .await
        .unwrap();
    assert_eq!(count.rows[0].get(0).map(Value::render), Some("2".into()));
}

/// C1 regression: a leading SQL comment must not hide the verb. The
/// pre-fix classifier scanned for the first alphabetic word after
/// `trim_start`, so `-- log\nINSERT …` looked like a no-op and the
/// `rows_affected` count was lost.
#[tokio::test]
#[ignore = "requires docker"]
async fn leading_comment_does_not_hide_mutating_verb() {
    let h = Harness::start().await;
    let mut conn = h.connect().await;

    conn.execute("CREATE TABLE dbo.cmt_t (id INT)", &[])
        .await
        .unwrap();

    let result = conn
        .execute(
            "-- written by smoke test\nINSERT INTO dbo.cmt_t VALUES (1), (2), (3)",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        result.rows_affected,
        Some(3),
        "rows_affected lost; leading comment hid the INSERT"
    );
}

/// M1 regression: `TcpStream::connect` to a black-holed address must
/// fail within the configured `connect_timeout`. We point at TEST-NET
/// (RFC 5737) port 1433; the SYN gets silently dropped, the OS would
/// otherwise retry for ~2 minutes.
#[tokio::test]
#[ignore = "requires docker"]
async fn connect_timeout_fires_on_blackholed_host() {
    use std::time::Instant;

    let mut options = std::collections::BTreeMap::new();
    options.insert("trust_server_certificate".into(), "true".into());
    options.insert("connect_timeout".into(), "2".into());
    let config = ConnectionConfig {
        id: Uuid::nil(),
        name: "blackhole".into(),
        driver: MssqlDriver::NAME.into(),
        // 192.0.2.0/24 is TEST-NET-1; routed to nothing.
        params: ConnectionParams::with(|p| {
            p.host = Some("192.0.2.1".into());
            p.port = Some(1433);
            p.database = Some("master".into());
            p.username = Some("sa".into());
            p.options = options;
        }),
    };

    let started = Instant::now();
    let outcome = MssqlDriver::new().connect(&config, Some("any")).await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "timeout was not honoured; elapsed {elapsed:?}"
    );
    match outcome {
        Ok(_) => panic!("expected timeout error against 192.0.2.1"),
        Err(err) => {
            let msg = err.to_string();
            assert!(
                msg.contains("timed out") || msg.contains("tcp connect"),
                "expected timeout/connect error, got: {msg}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires docker"]
async fn uniqueidentifier_round_trips_without_byte_swap() {
    // Classic data-corruption bug: SQL Server's `uniqueidentifier`
    // stores the first 8 bytes in mixed endianness vs RFC 4122. The
    // tiberius `Guid` <-> uuid::Uuid bridge is supposed to canonicalise
    // both directions; this test guards against a regression where we
    // forget to use tiberius' conversion and swap bytes manually.
    let h = Harness::start().await;
    let mut conn = h.connect().await;
    let id = uuid::uuid!("550e8400-e29b-41d4-a716-446655440000");

    conn.execute("CREATE TABLE g (id UNIQUEIDENTIFIER PRIMARY KEY)", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO g VALUES (@P1)", &[Value::Uuid(id)])
        .await
        .unwrap();

    let result = conn.execute("SELECT id FROM g", &[]).await.unwrap();
    assert_eq!(result.rows.len(), 1);
    match result.rows[0].get(0) {
        Some(Value::Uuid(got)) => assert_eq!(*got, id),
        other => panic!("expected Value::Uuid({id}), got {other:?}"),
    }
}

/// Optional smoke test against an externally-managed server. Set the
/// `NARWHAL_MSSQL_HOST`, `NARWHAL_MSSQL_USER`, `NARWHAL_MSSQL_PASSWORD`,
/// `NARWHAL_MSSQL_DATABASE` (optional, defaults to `master`) and
/// `NARWHAL_MSSQL_PORT` (optional, defaults to 1433) environment
/// variables to point this test at a long-lived dev instance or
/// Azure SQL DB without re-pulling the docker image.
#[tokio::test]
#[ignore = "requires NARWHAL_MSSQL_HOST"]
async fn external_select_one() {
    let host = match std::env::var("NARWHAL_MSSQL_HOST") {
        Ok(v) => v,
        Err(_) => return,
    };
    let user = std::env::var("NARWHAL_MSSQL_USER")
        .expect("NARWHAL_MSSQL_USER required when NARWHAL_MSSQL_HOST is set");
    let password = std::env::var("NARWHAL_MSSQL_PASSWORD").unwrap_or_default();
    let port: u16 = std::env::var("NARWHAL_MSSQL_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1433);
    let database = std::env::var("NARWHAL_MSSQL_DATABASE").unwrap_or_else(|_| "master".into());

    let mut options = std::collections::BTreeMap::new();
    options.insert("trust_server_certificate".into(), "true".into());
    let config = ConnectionConfig {
        id: Uuid::nil(),
        name: "env".into(),
        driver: MssqlDriver::NAME.into(),
        params: ConnectionParams::with(|p| {
            p.host = Some(host);
            p.port = Some(port);
            p.database = Some(database);
            p.username = Some(user);
            p.options = options;
        }),
    };
    let mut conn = MssqlDriver::new()
        .connect(&config, Some(&password))
        .await
        .expect("connect via env vars");
    let result = conn.execute("SELECT 1", &[]).await.unwrap();
    match result.rows[0].get(0) {
        Some(Value::Int(1)) => {}
        other => panic!("expected Value::Int(1), got {other:?}"),
    }
}
