//! Integration tests for the Lua-backed plugin pipeline inside AppCore.
//!
//! These tests build a real [`AppCore`] (no open connection — plugin
//! commands don't need one) and drive the `:` prompt through
//! [`AppCore::execute_command`]. The Lua scripts are written inline so
//! the test files stay self-contained.

use std::path::PathBuf;

use narwhal_app::core::AppCore;
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_plugin_lua::LuaPlugin;
use tempfile::TempDir;

fn empty_core() -> AppCore {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: Vec::new(),
    };
    AppCore::new(registry, connections, None)
}

fn write_script(dir: &TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, body).unwrap();
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plug_load_registers_lua_command_and_dispatches_it() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = write_script(
        &dir,
        "shout.lua",
        r#"
        narwhal.register_command("shout", "uppercase the argument", function(arg)
            return arg:upper()
        end)
        "#,
    );

    let mut core = empty_core();
    core.execute_command(&format!("plug-load {}", script_path.display()));
    assert!(
        core.status_message().contains("loaded"),
        "expected load confirmation, got: {}",
        core.status_message()
    );

    // The new command 'shout' should now be reachable from the `:` prompt.
    core.execute_command("shout hello berkant");
    assert_eq!(core.status_message(), "HELLO BERKANT");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plug_list_summarises_loaded_plugins() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = write_script(
        &dir,
        "two.lua",
        r#"
        narwhal.register_command("a", "alpha", function(_) return "a" end)
        narwhal.register_command("b", "beta",  function(_) return "b" end)
        "#,
    );

    let mut core = empty_core();
    core.execute_command(&format!("plug-load {}", script_path.display()));
    core.execute_command("plug-list");
    let msg = core.status_message();
    assert!(msg.contains("alpha"), "missing alpha: {msg}");
    assert!(msg.contains("beta"), "missing beta: {msg}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plug_list_with_no_plugins_hints_at_plug_load() {
    let mut core = empty_core();
    core.execute_command("plug-list");
    assert!(core.status_message().contains("plug-load"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plug_load_with_bad_script_reports_error_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = write_script(&dir, "broken.lua", "this is not valid lua )");

    let mut core = empty_core();
    core.execute_command(&format!("plug-load {}", script_path.display()));
    assert!(
        core.status_message().contains("failed"),
        "expected failure message, got: {}",
        core.status_message()
    );
    assert_eq!(core.plugins().plugins().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lua_command_returning_table_injects_sql_into_editor() {
    // Bypass `:plug-load` and register the plugin directly so the test
    // doesn't depend on tempfiles.
    let plugin = LuaPlugin::from_script(
        "snippets",
        r#"
        narwhal.register_command("count", "produce a count(*) query", function(arg)
            return { sql = "SELECT COUNT(*) FROM " .. arg, append = false }
        end)
        "#,
    )
    .unwrap();

    let mut core = empty_core();
    core.plugins_mut().register(plugin).unwrap();

    core.execute_command("count users");
    assert_eq!(core.editor().entire_text(), "SELECT COUNT(*) FROM users");
    assert!(
        core.status_message().contains("inserted"),
        "got: {}",
        core.status_message()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_command_path_still_reports_unknown_when_no_plugin_claims_it() {
    let mut core = empty_core();
    core.execute_command("this-name-does-not-exist arg");
    assert!(
        core.status_message().contains("unknown command"),
        "got: {}",
        core.status_message()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lua_handler_runtime_error_surfaces_as_plugin_error() {
    let plugin = LuaPlugin::from_script(
        "boom",
        r#"
        narwhal.register_command("boom", "explode", function(_)
            error("intentional")
        end)
        "#,
    )
    .unwrap();

    let mut core = empty_core();
    core.plugins_mut().register(plugin).unwrap();

    core.execute_command("boom");
    let msg = core.status_message();
    assert!(
        msg.contains("plugin error") || msg.contains("intentional"),
        "got: {msg}"
    );
}
