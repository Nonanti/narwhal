//! Postgres round-trip test for the schema-diff DDL emitter.
//!
//! The brief's acceptance criterion is "Postgres DDL emission
//! round-trips: apply emitted DDL to target, re-diff, expect empty".
//! This test does exactly that against an ephemeral Postgres
//! container:
//!
//! 1. Stand up two databases (source + target) by running different
//!    initial DDL on each.
//! 2. Introspect both via `Connection::list_all_tables` +
//!    `describe_table`.
//! 3. Run `narwhal_schema_diff::diff` to compute the migration.
//! 4. Render the migration via `PostgresEmitter`.
//! 5. Apply the emitted DDL to the target.
//! 6. Re-introspect the target. The post-migration diff against the
//!    source schema must be empty — proving the emitter's output is
//!    a complete fix-up, not just plausible-looking SQL.
//!
//! Requires Docker; marked `#[ignore]` to match the rest of the
//! postgres integration suite.
//!
//! Run with:
//! ```sh
//! cargo test -p narwhal-drivers --features postgres \
//!     postgres_schema_diff_roundtrip -- --ignored
//! ```

#![cfg(feature = "postgres")]

use narwhal_core::schema::TableSchema;
use narwhal_core::{ConnectionConfig, ConnectionParams, DatabaseDriver, DynConnection, TableKind};
use narwhal_drivers::postgres::PostgresDriver;
use narwhal_schema_diff::diff;
use narwhal_schema_diff::emit::{DdlEmitter, PostgresEmitter};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

/// Two Postgres databases on one container: `src_db` (desired) and
/// `tgt_db` (will be migrated). Sharing a single container keeps
/// the test cheap; the databases are isolated by name.
struct TwinHarness {
    _container: testcontainers::ContainerAsync<Postgres>,
    driver: PostgresDriver,
    src_config: ConnectionConfig,
    tgt_config: ConnectionConfig,
    password: String,
}

impl TwinHarness {
    async fn start() -> Self {
        let container = Postgres::default().start().await.expect("start postgres");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres port");

        let make_cfg = |name: &str, db: &str| ConnectionConfig {
            id: Uuid::new_v4(),
            name: name.into(),
            driver: PostgresDriver::NAME.into(),
            params: ConnectionParams::with(|p| {
                p.host = Some("127.0.0.1".into());
                p.port = Some(port);
                p.database = Some(db.into());
                p.username = Some("postgres".into());
            }),
        };

        let mut h = Self {
            _container: container,
            driver: PostgresDriver::new(),
            src_config: make_cfg("source", "src_db"),
            tgt_config: make_cfg("target", "tgt_db"),
            password: "postgres".into(),
        };
        // Use the admin DB to create the two test databases.
        let mut admin = make_cfg("admin", "postgres");
        admin.params.database = Some("postgres".into());
        let mut admin_conn = h
            .driver
            .connect(&admin, Some(&h.password))
            .await
            .expect("admin connect");
        admin_conn
            .execute("CREATE DATABASE src_db", &[])
            .await
            .expect("create src_db");
        admin_conn
            .execute("CREATE DATABASE tgt_db", &[])
            .await
            .expect("create tgt_db");
        let _ = admin_conn.close().await;
        // Touch the variant so clippy doesn't grumble about the
        // self-reference.
        h.password = h.password.clone();
        h
    }

    async fn connect_src(&self) -> Box<dyn DynConnection> {
        self.driver
            .connect(&self.src_config, Some(&self.password))
            .await
            .expect("connect source")
    }

    async fn connect_tgt(&self) -> Box<dyn DynConnection> {
        self.driver
            .connect(&self.tgt_config, Some(&self.password))
            .await
            .expect("connect target")
    }
}

async fn run(conn: &mut Box<dyn DynConnection>, sql: &str) {
    conn.execute(sql, &[])
        .await
        .unwrap_or_else(|e| panic!("execute `{sql}` failed: {e}"));
}

/// Walk every user table and return the introspected schemas.
async fn introspect(conn: &mut Box<dyn DynConnection>) -> Vec<TableSchema> {
    let catalog = conn.list_all_tables().await.expect("list_all_tables");
    let mut out = Vec::new();
    for (schema, tables) in catalog {
        for table in tables {
            if !matches!(table.kind, TableKind::Table) {
                continue;
            }
            // permission noise on system tables is ignored — the
            // round-trip only cares about user-visible objects.
            if let Ok(ts) = conn.describe_table(&schema.name, &table.name).await {
                out.push(ts);
            }
        }
    }
    out
}

#[tokio::test]
#[ignore = "requires docker"]
async fn schema_diff_postgres_round_trip() {
    let h = TwinHarness::start().await;

    // Source state: users(id, email NOT NULL, created_at DEFAULT now())
    let mut src = h.connect_src().await;
    run(
        &mut src,
        "CREATE TABLE users (
            id          serial PRIMARY KEY,
            email       text NOT NULL,
            created_at  timestamp DEFAULT now()
        )",
    )
    .await;
    run(&mut src, "CREATE INDEX users_email_idx ON users (email)").await;

    // Target state: missing email's NOT NULL, missing created_at,
    // missing index — the migration must add all three.
    let mut tgt = h.connect_tgt().await;
    run(
        &mut tgt,
        "CREATE TABLE users (
            id      serial PRIMARY KEY,
            email   text
        )",
    )
    .await;

    let source_tables = introspect(&mut src).await;
    let target_tables = introspect(&mut tgt).await;

    let initial = diff(&source_tables, &target_tables);
    assert!(
        !initial.is_empty(),
        "test precondition: the schemas must differ before the round-trip"
    );

    let ddl = PostgresEmitter::new()
        .emit(&initial)
        .expect("emit postgres DDL");

    // Apply the migration to the target. Each statement is run
    // independently so a syntax error names the offending line.
    for stmt in split_statements(&ddl) {
        run(&mut tgt, &stmt).await;
    }

    // Re-introspect; the post-migration diff must be empty.
    let target_after = introspect(&mut tgt).await;
    let after = diff(&source_tables, &target_after);
    assert!(
        after.is_empty(),
        "post-migration diff must be empty.\nDDL applied:\n{ddl}\nstill-differing:\n{after:#?}",
    );

    let _ = src.close().await;
    let _ = tgt.close().await;
}

/// Split rendered DDL into independently-executable statements.
///
/// The emitter terminates every statement with `;\n` and leaves
/// `-- comment` lines on their own. CREATE TABLE blocks span
/// multiple lines, so we buffer until we see a `;` at end-of-line.
fn split_statements(ddl: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for line in ddl.split('\n') {
        if line.trim_start().starts_with("--") || line.trim().is_empty() {
            continue;
        }
        buf.push_str(line);
        buf.push('\n');
        if line.trim_end().ends_with(';') {
            out.push(std::mem::take(&mut buf));
        }
    }
    out
}
