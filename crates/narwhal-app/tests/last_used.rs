//! Last-used recency ordering for the sidebar.
//!
//! Opens two saved sqlite connections in a deterministic order and
//! asserts the sidebar then lists them most-recently-opened first.

use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use narwhal_app::core::{AppCore, SidebarItem};
use narwhal_app::DriverRegistry;
use narwhal_config::{ConnectionsFile, LastUsedStore};
use narwhal_core::{ConnectionConfig, ConnectionParams};
use tempfile::TempDir;
use uuid::Uuid;

fn two_sqlite_fixtures(dir: &std::path::Path) -> (DriverRegistry, ConnectionsFile, [Uuid; 2]) {
    let a = ConnectionConfig {
        id: Uuid::new_v4(),
        name: "alpha".into(),
        driver: "sqlite".into(),
        params: ConnectionParams::with(|p| {
            p.path = Some(dir.join("a.db").to_string_lossy().into_owned());
        }),
    };
    let b = ConnectionConfig {
        id: Uuid::new_v4(),
        name: "bravo".into(),
        driver: "sqlite".into(),
        params: ConnectionParams::with(|p| {
            p.path = Some(dir.join("b.db").to_string_lossy().into_owned());
        }),
    };
    let ids = [a.id, b.id];
    let connections = ConnectionsFile {
        logical_relations: Vec::new(),
        connections: vec![a, b],
    };
    rusqlite::Connection::open(dir.join("a.db")).unwrap();
    rusqlite::Connection::open(dir.join("b.db")).unwrap();
    (DriverRegistry::with_defaults(), connections, ids)
}

fn first_connection_name(core: &AppCore) -> String {
    for item in core
        .last_layout()
        .sidebar_tables
        .iter()
        .map(|(_, idx)| *idx)
    {
        let _ = item;
    }
    // Walk the items list (`SidebarItem::Connection` variants only)
    // because `last_layout` is empty before the first render.
    for item in core.sidebar_items_for_test() {
        if let SidebarItem::Connection { name, .. } = item {
            return name.clone();
        }
    }
    panic!("no connection items in sidebar");
}

/// Without any history, ordering is alphabetical (alpha before bravo).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_install_falls_back_to_alphabetical() {
    let dir = TempDir::new().unwrap();
    let (registry, connections, _) = two_sqlite_fixtures(dir.path());
    let core = AppCore::new(registry, connections, None);
    assert_eq!(first_connection_name(&core), "alpha");
}

/// After opening `bravo`, it should jump above `alpha` in the
/// sidebar.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_promotes_connection_to_top() {
    let dir = TempDir::new().unwrap();
    let (registry, connections, _) = two_sqlite_fixtures(dir.path());
    let mut core = AppCore::new(registry, connections, None);

    // Wire the cache to disk so set_last_used_path also exercises load.
    let last_used = dir.path().join("last_used.toml");
    core.set_last_used_path(last_used.clone());

    core.execute_command("open bravo").await;
    assert!(core.session().is_some());
    assert_eq!(first_connection_name(&core), "bravo");

    // The cache must have been written through.
    let loaded = LastUsedStore::load(&last_used).unwrap();
    let bravo_id = core
        .connections()
        .iter()
        .find(|c| c.name == "bravo")
        .unwrap()
        .id;
    assert!(loaded.get(bravo_id).is_some());
}

/// Opening alpha *after* bravo flips the order back: alpha rises to
/// the top because it has the freshest timestamp.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn most_recently_opened_wins() {
    let dir = TempDir::new().unwrap();
    let (registry, connections, _) = two_sqlite_fixtures(dir.path());
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("open alpha").await;
    // touch_last_used uses millisecond timestamps; sleep a tick to
    // make sure the two opens land in distinct buckets.
    sleep(Duration::from_millis(5));
    core.execute_command("close").await;
    core.execute_command("open bravo").await;
    sleep(Duration::from_millis(5));
    core.execute_command("close").await;
    core.execute_command("open alpha").await;

    assert_eq!(first_connection_name(&core), "alpha");
}

/// `:remove` clears the recency entry so a re-added connection with
/// the same name (but new uuid) starts fresh.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_clears_last_used_entry() {
    let dir = TempDir::new().unwrap();
    let (registry, connections, _) = two_sqlite_fixtures(dir.path());
    let mut core = AppCore::new(registry, connections, None);

    let last_used = dir.path().join("last_used.toml");
    core.set_last_used_path(last_used.clone());

    core.execute_command("open bravo").await;
    let bravo_id = core
        .connections()
        .iter()
        .find(|c| c.name == "bravo")
        .unwrap()
        .id;
    assert!(LastUsedStore::load(&last_used)
        .unwrap()
        .get(bravo_id)
        .is_some());

    core.execute_command("close").await;
    core.execute_command("rm bravo").await;
    assert!(LastUsedStore::load(&last_used)
        .unwrap()
        .get(bravo_id)
        .is_none());
}

/// Smoke for the path-based opener with no on-disk wiring: even when
/// the cache stays in memory the ordering still updates within the
/// session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_memory_ordering_works_without_disk() {
    let dir = TempDir::new().unwrap();
    let (registry, connections, _) = two_sqlite_fixtures(dir.path());
    let mut core = AppCore::new(registry, connections, None);

    core.execute_command("open bravo").await;
    assert_eq!(first_connection_name(&core), "bravo");
}

/// Sanity: a fresh `AppCore` returns the two sqlite fixtures and nothing
/// else (no schemas yet since we haven't opened anything).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_round_trip() {
    let dir = TempDir::new().unwrap();
    let (registry, connections, _) = two_sqlite_fixtures(dir.path());
    let core = AppCore::new(registry, connections, None);
    let names: Vec<_> = core
        .sidebar_items_for_test()
        .iter()
        .filter_map(|i| match i {
            SidebarItem::Connection { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["alpha".to_owned(), "bravo".to_owned()]);
}

// Avoid an unused-import warning when only some tests touch PathBuf.
#[allow(dead_code)]
fn _force_use(_: PathBuf) {}
