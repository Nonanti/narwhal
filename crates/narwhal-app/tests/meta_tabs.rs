//! Integration tests for the L36 metadata tab strip:
//! `Records / Columns / Constraints / FKs / Indexes` chord-bound to
//! `1`..=`5` on the Results pane.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::DriverRegistry;
use narwhal_app::core::{AppCore, ResultState};
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use narwhal_tui::{MetaTab, Pane};
use tempfile::TempDir;
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        schema_version: None,
        logical_relations: Vec::new(),
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "headless".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(database_path.to_string_lossy().into_owned());
            }),
        }],
    };
    (registry, connections)
}

const fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// Seed an `orders` table with a PK + FK + secondary index so every
/// metadata tab has something interesting to show.
async fn seed_orders(db_path: &std::path::Path) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE customers (
             id   INTEGER PRIMARY KEY,
             name TEXT    NOT NULL UNIQUE
         );
         CREATE TABLE orders (
             id          INTEGER PRIMARY KEY,
             customer_id INTEGER NOT NULL REFERENCES customers(id),
             total       REAL    NOT NULL
         );
         CREATE INDEX orders_customer_idx ON orders(customer_id);
         INSERT INTO customers (id, name) VALUES (1, 'alice'), (2, 'bob');
         INSERT INTO orders (id, customer_id, total) VALUES (10, 1, 99.5), (11, 2, 12.0);",
    )
    .unwrap();
}

/// Drive the sidebar so a fresh `AppCore` is parked on the `orders`
/// row, then press Enter to open the metadata view.
async fn enter_orders_detail(core: &mut AppCore) {
    let ctrl_w = KeyEvent {
        code: KeyCode::Char('w'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    while core.focus() != Pane::Sidebar {
        core.handle_key(ctrl_w).await;
    }
    // Sidebar layout: connection(0) → main(1) → customers(2) → orders(3).
    for _ in 0..3 {
        core.handle_key(key(KeyCode::Char('j'))).await;
    }
    core.handle_key(key(KeyCode::Enter)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_table_lands_on_columns_tab_and_focuses_results() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("m.db");
    seed_orders(&db_path).await;

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open headless").await;
    enter_orders_detail(&mut core).await;

    // Focus should have moved to Results automatically so the numeric
    // chords work without an extra Ctrl-W.
    assert_eq!(core.focus(), Pane::Results, "focus must move to Results");
    match core.result() {
        ResultState::TableDetail {
            schema,
            active_meta_tab,
        } => {
            assert_eq!(schema.table.name, "orders");
            assert_eq!(
                *active_meta_tab,
                MetaTab::Columns,
                "default tab must be Columns"
            );
        }
        other => panic!("expected TableDetail, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn numeric_chords_switch_between_meta_tabs() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("m.db");
    seed_orders(&db_path).await;

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open headless").await;
    enter_orders_detail(&mut core).await;

    // 3 → Constraints
    core.handle_key(key(KeyCode::Char('3'))).await;
    assert!(
        matches!(
            core.result(),
            ResultState::TableDetail {
                active_meta_tab: MetaTab::Constraints,
                ..
            }
        ),
        "3 should switch to Constraints"
    );

    // 4 → ForeignKeys
    core.handle_key(key(KeyCode::Char('4'))).await;
    assert!(matches!(
        core.result(),
        ResultState::TableDetail {
            active_meta_tab: MetaTab::ForeignKeys,
            ..
        }
    ));

    // 5 → Indexes
    core.handle_key(key(KeyCode::Char('5'))).await;
    assert!(matches!(
        core.result(),
        ResultState::TableDetail {
            active_meta_tab: MetaTab::Indexes,
            ..
        }
    ));

    // 2 → back to Columns
    core.handle_key(key(KeyCode::Char('2'))).await;
    assert!(matches!(
        core.result(),
        ResultState::TableDetail {
            active_meta_tab: MetaTab::Columns,
            ..
        }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn records_chord_swaps_table_detail_for_a_row_preview() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("m.db");
    seed_orders(&db_path).await;

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open headless").await;
    enter_orders_detail(&mut core).await;
    // Records (1) dispatches a SELECT * preview that lands as Rows.
    core.handle_key(key(KeyCode::Char('1'))).await;
    core.drain_run_updates().await;

    match core.result() {
        ResultState::Rows {
            rows,
            source: Some(src),
            ..
        } => {
            assert_eq!(src.table, "orders");
            assert!(!rows.is_empty(), "preview must return seeded rows");
        }
        other => panic!("expected Rows preview, got {other:?}"),
    }

    // Then '4' (FKs) should re-describe and land in the FKs sub-view.
    core.handle_key(key(KeyCode::Char('4'))).await;
    match core.result() {
        ResultState::TableDetail {
            active_meta_tab: MetaTab::ForeignKeys,
            schema,
        } => {
            assert_eq!(schema.foreign_keys.len(), 1);
            assert_eq!(schema.foreign_keys[0].referenced_table, "customers");
        }
        other => panic!("expected TableDetail/FKs, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn meta_tab_chord_without_table_is_a_no_op_hint() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open headless").await;

    // Force focus to Results without a table open.
    let ctrl_w = KeyEvent {
        code: KeyCode::Char('w'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };
    while core.focus() != Pane::Results {
        core.handle_key(ctrl_w).await;
    }
    core.handle_key(key(KeyCode::Char('3'))).await;

    assert!(
        matches!(core.result(), ResultState::Empty),
        "no table → no state change"
    );
    let msg = core.status_message();
    assert!(
        msg.contains("Constraints") && msg.contains("sidebar"),
        "status should hint about opening a table; got: {msg}"
    );
}
