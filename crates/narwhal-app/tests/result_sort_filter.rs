//! Integration tests for result sort and filter (plan 06-04).
//!
//! Each test creates an `AppCore` with an in-memory `SQLite` session,
//! seeds a result set, then drives key events to exercise sort toggle,
//! filter prompt, and streaming guard.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use narwhal_app::core::{AppCore, ResultState};
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ColumnHeader, ConnectionConfig, ConnectionParams, Row};
use narwhal_tui::{Pane, ResultView, SortDir};
use tempfile::TempDir;
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "sort-filter-test".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(database_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
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

const fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// Focus the result pane via Ctrl-W cycling.
async fn focus_results(core: &mut AppCore) {
    let ctrl_w = ctrl(KeyCode::Char('w'));
    while core.focus() != Pane::Results {
        core.handle_key(ctrl_w).await;
    }
}

/// Seed a 5-row result set with (id INT, label TEXT) and focus results.
async fn seed_result(core: &mut AppCore, db_path: PathBuf) {
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT);
             INSERT INTO items VALUES (3, 'cherry');
             INSERT INTO items VALUES (1, 'apple');
             INSERT INTO items VALUES (5, 'elderberry');
             INSERT INTO items VALUES (2, 'banana');
             INSERT INTO items VALUES (4, 'date');",
        )
        .unwrap();
    }
    core.execute_command("open sort-filter-test").await;
    core.insert_into_editor("SELECT id, label FROM items ORDER BY label")
        .await;
    core.execute_command("run").await;
    core.drain_run_updates().await;
    focus_results(core).await;
}

/// Compute visible row indices directly from result state + `ResultView`,
/// without needing a render step. This mirrors
/// `ResultView::visible_rows`.
fn compute_visible(columns: &[ColumnHeader], rows: &[Row], view: &ResultView) -> Vec<usize> {
    view.visible_rows(columns, rows)
}

/// Get the active tab's result columns and rows.
fn get_rows(core: &AppCore) -> (Vec<ColumnHeader>, Vec<Row>) {
    match core.tabs()[core.active_tab()].results().active_state() {
        ResultState::Rows { columns, rows, .. } => (columns.clone(), rows.clone()),
        _ => panic!("expected Rows"),
    }
}

/// Get a reference to the active tab's `ResultView`.
fn result_view(core: &AppCore) -> &ResultView {
    core.tabs()[core.active_tab()].results().active()
}

/// Extract integer ids from visible row indices.
fn extract_ids(vis: &[usize], rows: &[Row], col: usize) -> Vec<i64> {
    vis.iter()
        .map(|&i| match &rows[i].0[col] {
            narwhal_core::Value::Int(n) => *n,
            other => panic!("expected Int, got {other:?}"),
        })
        .collect()
}

/// Extract rendered strings from visible row indices.
fn extract_rendered(vis: &[usize], rows: &[Row], col: usize) -> Vec<String> {
    vis.iter().map(|&i| rows[i].0[col].render()).collect()
}

// Test 1: sort cycle None → Asc → Desc → None

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sort_asc_then_desc_then_off() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;

    let (columns, rows) = get_rows(&core);

    // With ORDER BY label, rows arrive as:
    //   rows[0] = (1, apple), rows[1] = (2, banana), rows[2] = (3, cherry),
    //   rows[3] = (4, date), rows[4] = (5, elderberry)
    // Natural (no-sort) id order: [1, 2, 3, 4, 5]

    // First `s`: None → Asc (by id, column 0)
    core.handle_key(key(KeyCode::Char('s'))).await;
    assert_eq!(result_view(&core).sort, Some((0, SortDir::Asc)));
    let vis = compute_visible(&columns, &rows, result_view(&core));
    assert_eq!(
        extract_ids(&vis, &rows, 0),
        vec![1, 2, 3, 4, 5],
        "ascending by id"
    );

    // Second `s`: Asc → Desc
    core.handle_key(key(KeyCode::Char('s'))).await;
    assert_eq!(result_view(&core).sort, Some((0, SortDir::Desc)));
    let vis = compute_visible(&columns, &rows, result_view(&core));
    assert_eq!(
        extract_ids(&vis, &rows, 0),
        vec![5, 4, 3, 2, 1],
        "descending by id"
    );

    // Third `s`: Desc → None (back to SQL order)
    core.handle_key(key(KeyCode::Char('s'))).await;
    assert_eq!(result_view(&core).sort, None);
    let vis = compute_visible(&columns, &rows, result_view(&core));
    assert_eq!(
        extract_ids(&vis, &rows, 0),
        vec![1, 2, 3, 4, 5],
        "sort cleared, SQL order"
    );
}

// Test 2: stable sort across ties

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sort_stable_across_ties() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE fruits (id INTEGER PRIMARY KEY, category TEXT, name TEXT);
             INSERT INTO fruits VALUES (1, 'citrus', 'lemon');
             INSERT INTO fruits VALUES (2, 'berry', 'blueberry');
             INSERT INTO fruits VALUES (3, 'citrus', 'orange');
             INSERT INTO fruits VALUES (4, 'berry', 'strawberry');",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open sort-filter-test").await;
    core.insert_into_editor("SELECT id, category, name FROM fruits ORDER BY id")
        .await;
    core.execute_command("run").await;
    core.drain_run_updates().await;
    focus_results(&mut core).await;

    // Move to column 1 (category) and sort ascending.
    core.handle_key(key(KeyCode::Char('l'))).await;
    core.handle_key(key(KeyCode::Char('s'))).await;

    let (columns, rows) = get_rows(&core);
    let vis = compute_visible(&columns, &rows, result_view(&core));

    // berry rows should come before citrus rows, and within each group
    // the original insertion order must be preserved (id 2 before 4,
    // id 1 before 3).
    let categories: Vec<String> = vis.iter().map(|&i| rows[i].0[1].render()).collect();
    let ids = extract_ids(&vis, &rows, 0);

    // berry (ids 2,4) then citrus (ids 1,3) — stable within ties.
    assert_eq!(categories, vec!["berry", "berry", "citrus", "citrus"]);
    assert_eq!(
        ids,
        vec![2, 4, 1, 3],
        "stable sort preserves insertion order within ties"
    );
}

// Test 3: NULLs sort last in Asc, first in Desc

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sort_handles_nulls() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE nullable (id INTEGER PRIMARY KEY, val INTEGER);
             INSERT INTO nullable VALUES (1, 30);
             INSERT INTO nullable VALUES (2, NULL);
             INSERT INTO nullable VALUES (3, 10);
             INSERT INTO nullable VALUES (4, NULL);
             INSERT INTO nullable VALUES (5, 20);",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open sort-filter-test").await;
    core.insert_into_editor("SELECT id, val FROM nullable ORDER BY id")
        .await;
    core.execute_command("run").await;
    core.drain_run_updates().await;
    focus_results(&mut core).await;

    // Move to column 1 (val) and sort ascending.
    core.handle_key(key(KeyCode::Char('l'))).await;
    core.handle_key(key(KeyCode::Char('s'))).await;

    let (columns, rows) = get_rows(&core);
    let vis = compute_visible(&columns, &rows, result_view(&core));

    // Ascending: 10, 20, 30, NULL, NULL (nulls last)
    let vals = extract_rendered(&vis, &rows, 1);
    assert_eq!(
        vals,
        vec!["10", "20", "30", "NULL", "NULL"],
        "nulls sort last in ascending"
    );

    // Toggle to Descending: NULL, NULL, 30, 20, 10 (nulls first)
    core.handle_key(key(KeyCode::Char('s'))).await;
    let vis = compute_visible(&columns, &rows, result_view(&core));
    let vals = extract_rendered(&vis, &rows, 1);
    assert_eq!(
        vals,
        vec!["NULL", "NULL", "30", "20", "10"],
        "nulls sort first in descending"
    );
}

// Test 4: filter substring case-insensitive

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filter_substring_case_insensitive() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE words (id INTEGER PRIMARY KEY, word TEXT);
             INSERT INTO words VALUES (1, 'apple');
             INSERT INTO words VALUES (2, 'PENGUIN');
             INSERT INTO words VALUES (3, 'open');
             INSERT INTO words VALUES (4, 'banana');",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open sort-filter-test").await;
    core.insert_into_editor("SELECT id, word FROM words ORDER BY id")
        .await;
    core.execute_command("run").await;
    core.drain_run_updates().await;
    focus_results(&mut core).await;

    // Open filter prompt and type "PEN"
    core.handle_key(key(KeyCode::Char('/'))).await;
    for ch in "PEN".chars() {
        core.handle_key(key(KeyCode::Char(ch))).await;
    }
    // Accept with Enter
    core.handle_key(key(KeyCode::Enter)).await;

    let (columns, rows) = get_rows(&core);
    let vis = compute_visible(&columns, &rows, result_view(&core));

    // "PENGUIN" (id 2) and "open" (id 3) contain "pen" (case-insensitive)
    assert_eq!(
        extract_ids(&vis, &rows, 0),
        vec![2, 3],
        "filter matches case-insensitive 'pen' in 'PENGUIN' and 'open'"
    );
}

// Test 5: filter then sort

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filter_then_sort() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT, price INTEGER);
             INSERT INTO products VALUES (1, 'pen blue', 5);
             INSERT INTO products VALUES (2, 'pencil red', 3);
             INSERT INTO products VALUES (3, 'pen black', 8);
             INSERT INTO products VALUES (4, 'notebook', 12);
             INSERT INTO products VALUES (5, 'pen red', 2);",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open sort-filter-test").await;
    core.insert_into_editor("SELECT id, name, price FROM products ORDER BY id")
        .await;
    core.execute_command("run").await;
    core.drain_run_updates().await;
    focus_results(&mut core).await;

    // Filter to rows containing "pen"
    core.handle_key(key(KeyCode::Char('/'))).await;
    for ch in "pen".chars() {
        core.handle_key(key(KeyCode::Char(ch))).await;
    }
    core.handle_key(key(KeyCode::Enter)).await;

    // Now sort by column 2 (price) ascending.
    // Move right twice to column index 2.
    core.handle_key(key(KeyCode::Char('l'))).await;
    core.handle_key(key(KeyCode::Char('l'))).await;
    core.handle_key(key(KeyCode::Char('s'))).await;

    let (columns, rows) = get_rows(&core);
    let vis = compute_visible(&columns, &rows, result_view(&core));

    // Filtered rows containing "pen": pen blue (id 1, price 5),
    // pen black (id 3, price 8), pencil red (id 2, price 3),
    // pen red (id 5, price 2).
    // Sorted by price ascending: id 5 (2), id 2 (3), id 1 (5), id 3 (8)
    assert_eq!(
        extract_ids(&vis, &rows, 0),
        vec![5, 2, 1, 3],
        "filter then sort: 'pen*' sorted by price ascending"
    );
}

// Test 6: Escape clears filter and closes prompt

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn escape_clears_filter_and_closes_prompt() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    let (registry, connections) = fixture(db_path.clone());
    let mut core = AppCore::new(registry, connections, None);
    seed_result(&mut core, db_path).await;

    // Open filter prompt and type something
    core.handle_key(key(KeyCode::Char('/'))).await;
    assert!(
        core.tabs()[core.active_tab()]
            .results()
            .active()
            .filter_prompt_open
    );
    for ch in "abc".chars() {
        core.handle_key(key(KeyCode::Char(ch))).await;
    }
    assert_eq!(
        core.tabs()[core.active_tab()].results().active().filter,
        "abc"
    );

    // Press Esc — should clear filter and close prompt
    core.handle_key(key(KeyCode::Esc)).await;
    assert!(core.tabs()[core.active_tab()]
        .results()
        .active()
        .filter
        .is_empty());
    assert!(
        !core.tabs()[core.active_tab()]
            .results()
            .active()
            .filter_prompt_open
    );
    assert!(core.status_message().contains("filter cleared"));
}

// Test 7: streaming results reject sort

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_results_reject_sort() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE big (id INTEGER PRIMARY KEY, v TEXT);
             INSERT INTO big WITH RECURSIVE c(x) AS (
               SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 50
             ) SELECT x, 'row_' || x FROM c;",
        )
        .unwrap();
    }
    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open sort-filter-test").await;
    core.insert_into_editor("SELECT * FROM big").await;
    core.execute_command("stream").await;
    // Don't drain — we want the core in running state.
    assert!(core.is_running(), "core should be running during stream");

    focus_results(&mut core).await;

    // Try to sort — should be rejected.
    core.handle_key(key(KeyCode::Char('s'))).await;
    assert!(
        core.status_message()
            .contains("sort/filter unavailable while streaming"),
        "expected streaming guard message, got: {}",
        core.status_message()
    );

    // Try to filter — should also be rejected.
    core.handle_key(key(KeyCode::Char('/'))).await;
    assert!(
        core.status_message()
            .contains("sort/filter unavailable while streaming"),
        "expected streaming guard message for filter, got: {}",
        core.status_message()
    );

    // Clean up: drain the stream.
    core.drain_run_updates().await;
}
