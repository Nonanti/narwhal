//! `:format` / `:format-all` integration smoke.
//!
//! Drives the formatter through `AppCore::execute_command` so we
//! exercise the dispatcher + dialect-resolution + editor-rebuild
//! pipeline end-to-end without a terminal.

use std::path::PathBuf;

use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use uuid::Uuid;

fn fixture(database_path: PathBuf) -> (DriverRegistry, ConnectionsFile) {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "headless".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(database_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }],
    };
    (registry, connections)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn format_all_uppercases_and_indents_every_statement() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    core.insert_into_editor(
        "select id,name from users where active=true; select count(*) from orders",
    )
    .await;

    core.execute_command("format-all").await;
    let out = core.editor().entire_text();
    assert!(out.contains("SELECT"));
    assert!(out.contains("FROM"));
    assert!(out.contains("WHERE"));
    // Two statements → blank-line separated.
    assert!(out.contains("\n\n"));
    // Both terminate with semicolons.
    assert_eq!(out.matches(';').count(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn format_targets_only_the_current_statement() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    // `insert_into_editor` leaves the cursor at the end of the buffer,
    // so the second statement is the one under the cursor and the
    // formatter touches it while leaving the first one verbatim.
    core.insert_into_editor("select id from a;\n\nselect name from b")
        .await;
    core.execute_command("format").await;
    let out = core.editor().entire_text();
    assert!(
        out.contains("select id from a"),
        "first statement must survive untouched, got: {out}"
    );
    assert!(
        out.contains("SELECT") && out.contains("FROM"),
        "second statement must be uppercased, got: {out}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn format_on_empty_buffer_is_a_noop_with_friendly_status() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("format").await;
    assert!(core.editor().entire_text().is_empty());
    assert!(core.status_message().contains("nothing"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fmt_alias_resolves_to_format() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    core.insert_into_editor("select 1").await;
    core.execute_command("fmt").await;
    assert!(core
        .editor()
        .entire_text()
        .to_uppercase()
        .contains("SELECT"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fmtall_alias_resolves_to_format_all() {
    let (registry, connections) = fixture(PathBuf::from(":memory:"));
    let mut core = AppCore::new(registry, connections, None);
    core.insert_into_editor("select 1; select 2").await;
    core.execute_command("fmtall").await;
    let out = core.editor().entire_text();
    assert_eq!(out.matches(';').count(), 2);
}
