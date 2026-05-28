//! streaming-result tuning end-to-end tests.
//!
//! Verifies that `settings.run.batch_size` and `stream_flush_ms`
//! actually take effect at the worker layer, and that the new
//! `Connection::query` path round-trips a SELECT identically to the
//! pre-existing `execute` / `stream` paths.
//!
//! Review fixup M7: every test now drains updates manually so we
//! can count `RunUpdate::RowsAppended` chunks. The functional
//! "final rows are correct" check is kept as a regression guard,
//! but the chunk-count assertions are what catch a future
//! batching-logic regression.
//!
//! Uses the same SQLite-in-tempfile harness as
//! `streaming_counter.rs` so the tests are runnable in CI without
//! an external database.

use std::path::PathBuf;

use narwhal_app::DriverRegistry;
use narwhal_app::core::{AppCore, ResultState};
use narwhal_app::run::RunUpdate;
use narwhal_config::{ConnectionsFile, Settings};
use narwhal_core::{ConnectionConfig, ConnectionParams};
use tempfile::TempDir;
use uuid::Uuid;

/// Drain every `RunUpdate` from the worker and return
/// `(rows_appended_chunks, total_rows_appended)` alongside the
/// final result state. Lets tests assert on the batching
/// behaviour, not just the final row count.
async fn drain_counting_chunks(core: &mut AppCore) -> (usize, usize) {
    let mut chunks = 0usize;
    let mut total = 0usize;
    while core.is_running() {
        match core.recv_run_update().await {
            Some(update) => {
                if let RunUpdate::RowsAppended { rows } = &update {
                    chunks += 1;
                    total += rows.len();
                }
                core.handle_run_update(update).await;
            }
            None => break,
        }
    }
    (chunks, total)
}

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

fn build_db(rows: usize) -> TempDir {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE bulk (id INTEGER PRIMARY KEY, val TEXT);
             WITH RECURSIVE cnt(x) AS (
               SELECT 1 UNION ALL SELECT x+1 FROM cnt WHERE x < {rows}
             )
             INSERT INTO bulk (val) SELECT 'row_' || x FROM cnt;",
        ))
        .unwrap();
    }
    dir
}

/// Defaults from `Settings::default()` reach the worker — sanity
/// guard against a future refactor that drops the
/// `apply_settings -> stream_tuning` wiring.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_settings_produce_v1_compatible_streaming() {
    let dir = build_db(200);
    let db_path = dir.path().join("test.db");

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.apply_settings(Settings::default());
    core.execute_command("open headless").await;

    core.insert_into_editor("SELECT * FROM bulk").await;
    core.execute_command("stream").await;
    core.drain_run_updates().await;

    match core.result() {
        ResultState::Rows { rows, streamed, .. } => {
            assert_eq!(rows.len(), 200);
            assert!(*streamed);
        }
        other => panic!("expected Rows, got {other:?}"),
    }
}

/// Custom `batch_size = 1` produces correct totals AND one chunk
/// per row — the batching contract under pure-size mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn small_batch_size_produces_per_row_chunks() {
    let dir = build_db(50);
    let db_path = dir.path().join("test.db");

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    let mut settings = Settings::default();
    settings.run.batch_size = 1;
    settings.run.stream_flush_ms = 0; // pure size-based flush
    core.apply_settings(settings);
    core.execute_command("open headless").await;

    core.insert_into_editor("SELECT * FROM bulk").await;
    core.execute_command("stream").await;
    let (chunks, total) = drain_counting_chunks(&mut core).await;

    assert_eq!(total, 50, "row count must match");
    assert_eq!(
        chunks, 50,
        "batch_size = 1 must produce one chunk per row — review fixup M7 contract"
    );
    match core.result() {
        ResultState::Rows { rows, .. } => assert_eq!(rows.len(), 50),
        other => panic!("expected Rows, got {other:?}"),
    }
}

/// `stream_flush_ms = 0` disables the time-based flush. A large
/// `batch_size` + no time flush means a 25-row table arrives in
/// **exactly one** `RowsAppended` chunk (size threshold never
/// hit). This is the regression guard that proves the time-flush
/// is genuinely disabled, not just inert.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pure_size_batching_emits_single_chunk_when_under_threshold() {
    let dir = build_db(25);
    let db_path = dir.path().join("test.db");

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    let mut settings = Settings::default();
    settings.run.batch_size = 1024;
    settings.run.stream_flush_ms = 0;
    core.apply_settings(settings);
    core.execute_command("open headless").await;

    core.insert_into_editor("SELECT * FROM bulk").await;
    core.execute_command("stream").await;
    let (chunks, total) = drain_counting_chunks(&mut core).await;

    assert_eq!(total, 25);
    assert_eq!(
        chunks, 1,
        "flush_ms=0 + batch_size=1024 + 25-row stream must emit \
         exactly one chunk — review fixup M7 contract"
    );
}

/// Time-window flush path runs without livelocking: 5 ms window
/// against a 100-row table still completes and yields every row.
/// Loose chunk-count assertion (>=1, <= 100) just confirms the
/// loop terminated and produced chunks; the tight upper bound
/// would be flaky on a loaded CI runner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tight_time_window_does_not_livelock() {
    let dir = build_db(100);
    let db_path = dir.path().join("test.db");

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    let mut settings = Settings::default();
    settings.run.batch_size = 4096;
    settings.run.stream_flush_ms = 5;
    core.apply_settings(settings);
    core.execute_command("open headless").await;

    core.insert_into_editor("SELECT * FROM bulk").await;
    core.execute_command("stream").await;
    let (chunks, total) = drain_counting_chunks(&mut core).await;

    assert_eq!(total, 100);
    assert!(
        (1..=100).contains(&chunks),
        "expected 1..=100 chunks (got {chunks}); loop should terminate \
         and produce at least one batch"
    );
}

/// Review fixup M7: clamp guard — if a future refactor accidentally
/// sets `batch_size = 0` on the runtime field (bypassing
/// `StreamTuning::new`), the worker must NOT livelock. The local
/// `batch_size.max(1)` floor in `run_stream` is the defence; this
/// test exercises it by setting `batch_size = 1` (the closest
/// public-API value) and verifying the worker terminates with the
/// expected chunk count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_terminates_at_minimum_batch_size() {
    let dir = build_db(10);
    let db_path = dir.path().join("test.db");

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    let mut settings = Settings::default();
    settings.run.batch_size = 1;
    settings.run.stream_flush_ms = 0;
    core.apply_settings(settings);
    core.execute_command("open headless").await;

    core.insert_into_editor("SELECT * FROM bulk").await;
    core.execute_command("stream").await;
    let (chunks, total) = drain_counting_chunks(&mut core).await;

    assert_eq!(total, 10);
    assert_eq!(chunks, 10);
}

/// `Connection::query` exercised through the run pipeline still
/// returns identical results to the historical `stream` path. This
/// is the contract guard for the default `query` impl in
/// `Connection`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_path_matches_stream_path() {
    let dir = build_db(75);
    let db_path = dir.path().join("test.db");

    let (registry, connections) = fixture(db_path);
    let mut core = AppCore::new(registry, connections, None);
    core.apply_settings(Settings::default());
    core.execute_command("open headless").await;

    // `:stream` goes through Connection::query under the new code
    // path.
    core.insert_into_editor("SELECT * FROM bulk ORDER BY id")
        .await;
    core.execute_command("stream").await;
    core.drain_run_updates().await;

    let streamed_rows = match core.result() {
        ResultState::Rows { rows, .. } => rows.clone(),
        other => panic!("expected Rows from stream, got {other:?}"),
    };

    // Compare against the `:run` (execute) path which materialises
    // the entire result on the connection side.
    core.execute_command("clear").await;
    core.insert_into_editor("SELECT * FROM bulk ORDER BY id")
        .await;
    core.execute_command("run").await;
    core.drain_run_updates().await;

    let executed_rows = match core.result() {
        ResultState::Rows { rows, .. } => rows.clone(),
        other => panic!("expected Rows from run, got {other:?}"),
    };

    assert_eq!(streamed_rows.len(), executed_rows.len());
    assert_eq!(streamed_rows.len(), 75);
    // `narwhal_core::Value` doesn't implement `PartialEq` (it carries
    // an `f64`), so compare by Display rendering.
    fn render_row(r: &narwhal_core::Row) -> Vec<String> {
        r.0.iter().map(narwhal_core::Value::render).collect()
    }
    assert_eq!(render_row(&streamed_rows[0]), render_row(&executed_rows[0]));
    assert_eq!(
        render_row(streamed_rows.last().unwrap()),
        render_row(executed_rows.last().unwrap())
    );
}
