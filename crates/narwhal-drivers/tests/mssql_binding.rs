//! Pure-Rust tests for the MSSQL driver helpers exposed through the
//! `__test_only` module. These do not touch the network and run on the
//! default `cargo test` invocation.

#![cfg(feature = "mssql")]

use narwhal_core::{
    ConnectionConfig, ConnectionParams, Index, SslMode, TableKind, UniqueConstraint,
};
use narwhal_drivers::mssql::{__test_only, MssqlDriver};
use uuid::Uuid;

fn config(params: ConnectionParams) -> ConnectionConfig {
    ConnectionConfig {
        id: Uuid::nil(),
        name: "bind".into(),
        driver: MssqlDriver::NAME.into(),
        params,
    }
}

#[test]
fn build_config_default_port_is_1433() {
    let params = ConnectionParams::with(|p| {
        p.host = Some("db".into());
        p.username = Some("sa".into());
    });
    let cfg = __test_only::build_config(&config(params), Some("pw")).expect("build");
    assert_eq!(cfg.get_addr(), "db:1433");
}

#[test]
fn build_config_honours_explicit_port() {
    let params = ConnectionParams::with(|p| {
        p.host = Some("db".into());
        p.port = Some(14333);
        p.username = Some("sa".into());
    });
    let cfg = __test_only::build_config(&config(params), Some("pw")).expect("build");
    assert_eq!(cfg.get_addr(), "db:14333");
}

#[test]
fn build_config_ssl_mode_disable_drops_encryption() {
    let params = ConnectionParams::with(|p| {
        p.host = Some("db".into());
        p.username = Some("sa".into());
        p.ssl_mode = SslMode::Disable;
    });
    // We can't read EncryptionLevel back through tiberius' public API,
    // but we can at least confirm the builder doesn't blow up and
    // produces a usable config.
    let cfg = __test_only::build_config(&config(params), Some("pw")).expect("build");
    assert_eq!(cfg.get_addr(), "db:1433");
}

#[test]
fn map_table_kind_round_trip() {
    assert_eq!(__test_only::map_table_kind(Some("VIEW")), TableKind::View);
    assert_eq!(__test_only::map_table_kind(Some("V")), TableKind::View);
    assert_eq!(__test_only::map_table_kind(Some("U")), TableKind::Table);
    assert_eq!(__test_only::map_table_kind(None), TableKind::Table);
}

#[test]
fn unique_constraints_filter_pk_index() {
    let indexes = vec![
        Index {
            name: "PK_users".into(),
            columns: vec!["id".into()],
            unique: true,
            primary: true,
        },
        Index {
            name: "UQ_users_email".into(),
            columns: vec!["email".into()],
            unique: true,
            primary: false,
        },
    ];
    let uc: Vec<UniqueConstraint> = __test_only::unique_constraints_from_indexes(&indexes);
    assert_eq!(uc.len(), 1);
    assert_eq!(uc[0].name, "UQ_users_email");
    assert_eq!(uc[0].columns, vec!["email".to_owned()]);
}

/// Lock in the four routing shapes that downstream Tier-2 audit
/// snapshots depend on. The classifier replaced the v1
/// `is_mutating_statement` boolean because that path silently lost
/// rows from OUTPUT clauses, EXEC and mutating CTEs (T1-T2-A code
/// review, C1).
#[test]
fn classify_statement_routes_real_world_shapes() {
    use __test_only::StatementShape::*;

    // Pure read.
    for sql in [
        "SELECT 1",
        "  select * from t",
        "-- audit\nSELECT 1",
        "/* batch 7 */\nSELECT 1",
        "WITH cte AS (SELECT 1) SELECT * FROM cte",
        "DECLARE @x int",
        "SET NOCOUNT ON",
    ] {
        assert_eq!(
            __test_only::classify_statement(sql),
            Read,
            "expected Read: {sql}"
        );
    }

    // Pure mutating (must capture rows_affected).
    for sql in [
        "INSERT INTO t VALUES (1)",
        "  update t SET x = 1",
        "DELETE FROM t",
        "merge into t USING s ON 1=1 WHEN MATCHED THEN UPDATE SET x=1",
        "CREATE TABLE foo (id int)",
        "alter table t add column c int",
        "drop view v",
        "TRUNCATE TABLE t",
        "GRANT SELECT ON t TO public",
        // Leading comments must NOT hide the verb.
        "-- audit\nINSERT INTO t VALUES (1)",
        "/* batched */ UPDATE t SET x=1",
    ] {
        assert_eq!(
            __test_only::classify_statement(sql),
            Mutating,
            "expected Mutating: {sql}"
        );
    }

    // Mutating that also returns rows. Must preserve rows; tiberius'
    // QueryStream is the only API that does so.
    for sql in [
        "INSERT INTO t OUTPUT inserted.id VALUES (1)",
        "UPDATE t SET x=1 OUTPUT deleted.x, inserted.x WHERE id=1",
        "DELETE FROM t OUTPUT deleted.* WHERE id=1",
        "EXEC sp_who",
        "execute dbo.usp_lookup @id = 1",
        "WITH src AS (SELECT id FROM s) INSERT INTO t (id) SELECT id FROM src",
    ] {
        assert_eq!(
            __test_only::classify_statement(sql),
            MutatingWithRows,
            "expected MutatingWithRows: {sql}"
        );
    }
}

#[test]
fn classify_statement_ignores_keywords_inside_literals() {
    use __test_only::StatementShape::*;

    // The OUTPUT inside a string literal must not promote a plain
    // INSERT to MutatingWithRows — the row would be lost on the
    // rows_affected path otherwise.
    assert_eq!(
        __test_only::classify_statement("INSERT INTO log (msg) VALUES ('OUTPUT not a clause')"),
        Mutating
    );

    // Bracket-quoted identifier `[OUTPUT]` is not a clause.
    assert_eq!(
        __test_only::classify_statement("SELECT [OUTPUT] FROM t"),
        Read
    );
}

#[test]
fn leading_keyword_skips_comments() {
    assert_eq!(
        __test_only::leading_keyword("-- log line\nINSERT INTO t VALUES (1)"),
        Some("INSERT")
    );
    assert_eq!(
        __test_only::leading_keyword("/* hello */ UPDATE t SET x=1"),
        Some("UPDATE")
    );
    assert_eq!(__test_only::leading_keyword("   "), None);
}

#[test]
fn contains_top_level_keyword_respects_word_boundary() {
    // PASSOUTPUT is not OUTPUT; previously a substring match would
    // have promoted this to MutatingWithRows.
    assert!(!__test_only::contains_top_level_keyword(
        "SELECT PASSOUTPUT() FROM t",
        "OUTPUT"
    ));
    assert!(__test_only::contains_top_level_keyword(
        "INSERT INTO t OUTPUT inserted.id VALUES (1)",
        "OUTPUT"
    ));
}
