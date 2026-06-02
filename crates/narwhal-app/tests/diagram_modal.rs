//! End-to-end tests for the in-TUI `:diagram <table>` modal.
//!
//! Drives a real on-disk `SQLite` schema through the command palette,
//! then asserts the modal state (mode, cached model, neighbour cursor)
//! and the key vocabulary (`Tab`, `Enter`, `i`, `y`, `q`).

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use narwhal_app::core::{AppCore, DiagramMode};
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use tempfile::TempDir;
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        logical_relations: Vec::new(),
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "diagram-modal".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(database_path.to_string_lossy().into_owned());
            }),
        }],
    };
    (registry, connections)
}

fn seed(db_path: &PathBuf) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE users (
             id    INTEGER PRIMARY KEY,
             email TEXT NOT NULL UNIQUE
         );
         CREATE TABLE orders (
             id      INTEGER PRIMARY KEY,
             user_id INTEGER NOT NULL,
             status  TEXT,
             FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
         );
         CREATE TABLE order_items (
             order_id   INTEGER NOT NULL,
             product_id INTEGER NOT NULL,
             qty        INTEGER NOT NULL,
             PRIMARY KEY (order_id, product_id),
             FOREIGN KEY (order_id) REFERENCES orders(id) ON DELETE CASCADE
         );
         CREATE TABLE audit (
             id       INTEGER PRIMARY KEY,
             actor_id INTEGER,
             FOREIGN KEY (actor_id) REFERENCES users(id)
         );",
    )
    .unwrap();
}

async fn open_seeded_session() -> (TempDir, AppCore) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("schema.db");
    seed(&db_path);
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open diagram-modal").await;
    core.drain_run_updates_and_refresh().await;
    (dir, core)
}

const fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn focus_command_opens_modal_with_cached_model() {
    let (_dir, mut core) = open_seeded_session().await;

    core.execute_command("diagram orders").await;

    // modal accessor
    let state = core.diagram_for_test().expect("modal must be open");
    assert_eq!(state.mode, DiagramMode::Focused);
    assert_eq!(state.center.name, "orders");
    assert_eq!(state.center.schema, "main");
    // 4 tables in the fixture; the cached model holds all of them.
    assert_eq!(state.model.nodes.len(), 4);
    // 3 FK edges (orders\u2192users, order_items\u2192orders, audit\u2192users).
    assert_eq!(state.model.edges.len(), 3);
    assert_eq!(state.selected, 0);
    let status = core.status_message();
    assert!(status.contains("focused"), "status: {status}");
    assert!(status.contains("orders"), "status: {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn impact_command_opens_modal_with_reverse_fk_tree() {
    let (_dir, mut core) = open_seeded_session().await;

    core.execute_command("diagram impact users").await;

    // modal accessor
    let state = core.diagram_for_test().expect("modal must be open");
    assert_eq!(state.mode, DiagramMode::Impact);
    assert_eq!(state.center.name, "users");
    // users is referenced by orders + audit.
    let inbound_names: Vec<_> = state
        .impact
        .inbound
        .iter()
        .map(|n| n.table.name.clone())
        .collect();
    assert!(inbound_names.contains(&"orders".into()));
    assert!(inbound_names.contains(&"audit".into()));
    // orders is referenced by order_items \u2014 second level.
    let orders_children: Vec<_> = state
        .impact
        .inbound
        .iter()
        .find(|n| n.table.name == "orders")
        .unwrap()
        .children
        .iter()
        .map(|n| n.table.name.clone())
        .collect();
    assert_eq!(orders_children, vec!["order_items".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enter_recenters_to_selected_neighbour() {
    let (_dir, mut core) = open_seeded_session().await;
    core.execute_command("diagram orders").await;

    // Selection 0 = outbound[0] = users. Enter must re-centre on users.
    core.handle_key(key(KeyCode::Enter)).await;

    let state = core.diagram_for_test().expect("modal still open");
    assert_eq!(state.center.name, "users");
    assert_eq!(state.selected, 0, "selection resets on re-centre");
    let status = core.status_message();
    assert!(status.contains("re-centred"), "status: {status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tab_cycles_selection_and_wraps() {
    let (_dir, mut core) = open_seeded_session().await;
    // users has 0 outbound + 2 inbound (orders, audit) \u2192 2 navigable.
    core.execute_command("diagram users").await;
    assert_eq!(core.diagram_for_test().unwrap().selected, 0);

    core.handle_key(key(KeyCode::Tab)).await;
    assert_eq!(core.diagram_for_test().unwrap().selected, 1);

    // Wrap back to 0.
    core.handle_key(key(KeyCode::Tab)).await;
    assert_eq!(core.diagram_for_test().unwrap().selected, 0);

    // BackTab goes the other way.
    core.handle_key(key(KeyCode::BackTab)).await;
    assert_eq!(core.diagram_for_test().unwrap().selected, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn i_toggles_between_focused_and_impact() {
    let (_dir, mut core) = open_seeded_session().await;
    core.execute_command("diagram orders").await;

    core.handle_key(key(KeyCode::Char('i'))).await;
    assert_eq!(core.diagram_for_test().unwrap().mode, DiagramMode::Impact);

    core.handle_key(key(KeyCode::Char('i'))).await;
    assert_eq!(core.diagram_for_test().unwrap().mode, DiagramMode::Focused);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn y_yanks_mermaid_to_clipboard() {
    let (_dir, mut core) = open_seeded_session().await;
    core.execute_command("diagram orders").await;

    core.handle_key(key(KeyCode::Char('y'))).await;
    let status = core.status_message();
    assert!(status.contains("yanked mermaid"), "status: {status}");
    // We trust the export tests for the actual Mermaid content
    // assertions; here we only need to verify the wiring fired.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn q_and_esc_close_the_modal() {
    let (_dir, mut core) = open_seeded_session().await;
    core.execute_command("diagram orders").await;
    assert!(core.diagram_for_test().is_some());

    core.handle_key(key(KeyCode::Char('q'))).await;
    assert!(core.diagram_for_test().is_none());
    assert!(core.status_message().contains("closed"));

    // Re-open and try Esc.
    core.execute_command("diagram users").await;
    core.handle_key(key(KeyCode::Esc)).await;
    assert!(core.diagram_for_test().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_table_does_not_open_modal() {
    let (_dir, mut core) = open_seeded_session().await;

    core.execute_command("diagram nope").await;
    assert!(core.diagram_for_test().is_none());
    assert!(core.status_message().contains("not found"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sidebar_gd_chord_opens_focused_modal() {
    use narwhal_app::core::SidebarItem;
    let (_dir, mut core) = open_seeded_session().await;

    // Move focus to the sidebar and select the `orders` table.
    let orders_idx = core
        .ui_for_test()
        .sidebar_items
        .iter()
        .position(|item| matches!(item, SidebarItem::Table { name, .. } if name == "orders"))
        .expect("orders must be in the sidebar");
    core.set_sidebar_index_for_test(orders_idx);
    core.set_focus_sidebar_for_test();

    // First `g` arms the leader…
    core.handle_key(key(KeyCode::Char('g'))).await;
    assert!(core.diagram_for_test().is_none());
    // …then `d` opens the diagram.
    core.handle_key(key(KeyCode::Char('d'))).await;

    let state = core.diagram_for_test().expect("gd should open the modal");
    assert_eq!(state.center.name, "orders");
    assert_eq!(state.mode, DiagramMode::Focused);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sidebar_shift_d_also_opens_focused_modal() {
    use narwhal_app::core::SidebarItem;
    let (_dir, mut core) = open_seeded_session().await;

    let users_idx = core
        .ui_for_test()
        .sidebar_items
        .iter()
        .position(|item| matches!(item, SidebarItem::Table { name, .. } if name == "users"))
        .expect("users must be in the sidebar");
    core.set_sidebar_index_for_test(users_idx);
    core.set_focus_sidebar_for_test();

    core.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT))
        .await;

    let state = core
        .diagram_for_test()
        .expect("Shift+D should open the modal");
    assert_eq!(state.center.name, "users");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logical_relation_from_connections_toml_lands_on_model() {
    use narwhal_config::LogicalRelationConfig;
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("schema.db");
    seed(&db_path);

    // Build a `ConnectionsFile` with one connection plus a logical
    // relation declaring `audit.id → orders.id`. No workspace.toml.
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        logical_relations: vec![LogicalRelationConfig {
            connection: "diagram-modal".into(),
            from: Some("audit.id".into()),
            to: Some("orders.id".into()),
            cardinality: "many-to-one".into(),
            note: Some("reconciled async".into()),
            from_columns: Vec::new(),
            to_columns: Vec::new(),
        }],
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "diagram-modal".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(db_path.to_string_lossy().into_owned());
            }),
        }],
    };
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open diagram-modal").await;
    core.drain_run_updates_and_refresh().await;

    core.execute_command("diagram orders").await;
    let state = core.diagram_for_test().expect("modal open");
    // 3 FK edges + 1 logical edge.
    assert_eq!(state.model.edges.len(), 4, "logical edge must be wired");
    let logical = state
        .model
        .edges
        .iter()
        .find(|e| e.kind.is_logical())
        .expect("logical edge present");
    assert_eq!(logical.from.name, "audit");
    assert_eq!(logical.to.name, "orders");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_active_connection_friendly_error() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("schema.db");
    seed(&db_path);
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("diagram users").await;
    assert!(core.diagram_for_test().is_none());
    assert!(core.status_message().contains("no active connection"));
}
