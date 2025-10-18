//! Integration tests for the Lua-backed plugin pipeline inside AppCore.
//!
//! These tests build a real [`AppCore`] (no open connection — plugin
//! commands don't need one) and drive the `:` prompt through
//! [`AppCore::execute_command`]. The Lua scripts are written inline so
//! the test files stay self-contained.

use std::path::PathBuf;

use narwhal_app::core::{AppCore, ResultState};
use narwhal_app::DriverRegistry;
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use narwhal_plugin_lua::LuaPlugin;
use tempfile::TempDir;
use uuid::Uuid;

fn empty_core() -> AppCore {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: Vec::new(),
    };
    AppCore::new(registry, connections, None)
}

/// Build an AppCore wired to a freshly-seeded sqlite database with a
/// small `items` table, returning the core and the temp dir that owns
/// the file.
async fn core_with_items() -> (AppCore, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("p.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT)",
            [],
        )
        .unwrap();
        for (i, name) in ["alpha", "beta", "gamma"].iter().enumerate() {
            conn.execute(
                "INSERT INTO items (id, label) VALUES (?, ?)",
                rusqlite::params![(i + 1) as i64, name],
            )
            .unwrap();
        }
    }
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "p".into(),
            driver: "sqlite".into(),
            params: ConnectionParams {
                path: Some(db_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        }],
    };
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open p");
    (core, dir)
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
    core.register_lua_plugin(plugin).unwrap();

    core.execute_command("count users");
    assert_eq!(core.editor().entire_text(), "SELECT COUNT(*) FROM users");
    assert!(
        core.status_message().contains("inserted"),
        "got: {}",
        core.status_message()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_load_picks_up_every_lua_file_in_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    write_script(
        &dir,
        "a.lua",
        r#"narwhal.register_command("a", "alpha", function(_) return "A" end)"#,
    );
    write_script(
        &dir,
        "b.lua",
        r#"narwhal.register_command("b", "beta", function(_) return "B" end)"#,
    );
    // Decoy file: wrong extension, should be ignored.
    std::fs::write(dir.path().join("ignore.txt"), "not a plugin").unwrap();

    let mut core = empty_core();
    let loaded = core.auto_load_plugins(dir.path());
    assert_eq!(loaded, 2);
    assert!(core.status_message().contains("auto-loaded 2"));

    core.execute_command("a");
    assert_eq!(core.status_message(), "A");
    core.execute_command("b");
    assert_eq!(core.status_message(), "B");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_load_records_failing_scripts_without_aborting() {
    let dir = tempfile::tempdir().unwrap();
    write_script(
        &dir,
        "good.lua",
        r#"narwhal.register_command("good", "ok", function(_) return "good" end)"#,
    );
    write_script(&dir, "broken.lua", "this is not lua )");

    let mut core = empty_core();
    let loaded = core.auto_load_plugins(dir.path());
    assert_eq!(loaded, 1);

    // The good plugin works.
    core.execute_command("good");
    assert_eq!(core.status_message(), "good");

    // And the failure was recorded as a plugin warning that bubbles up
    // on the next AllDone-style status overwrite. We can't easily
    // observe that without a real query, so instead reach into the
    // registry to confirm exactly one plugin made it through.
    assert_eq!(core.plugins().plugins().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_load_missing_dir_is_silently_skipped() {
    let mut core = empty_core();
    let loaded = core.auto_load_plugins(std::path::Path::new("/definitely/does/not/exist"));
    assert_eq!(loaded, 0);
    // Status untouched (still 'ready').
    assert_eq!(core.status_message(), "ready");
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
async fn transform_hook_runs_after_query_and_rewrites_rows() {
    let plugin = LuaPlugin::from_script(
        "upper",
        r#"
        narwhal.register_transform(function(result)
            for _, row in ipairs(result.rows) do
                if type(row[2]) == "string" then
                    row[2] = row[2]:upper()
                end
            end
        end)
        "#,
    )
    .unwrap();

    let (mut core, _dir) = core_with_items().await;
    core.register_lua_plugin(plugin).unwrap();

    core.insert_into_editor("SELECT id, label FROM items ORDER BY id");
    core.execute_command("run");
    core.drain_run_updates().await;

    match core.result() {
        ResultState::Rows { rows, .. } => {
            // Plugin should have uppercased every label.
            assert_eq!(rows[0].0[1].render(), "ALPHA");
            assert_eq!(rows[1].0[1].render(), "BETA");
            assert_eq!(rows[2].0[1].render(), "GAMMA");
        }
        other => panic!("expected Rows, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transform_hook_can_add_synthetic_columns() {
    let plugin = LuaPlugin::from_script(
        "doubler",
        r#"
        narwhal.register_transform(function(result)
            table.insert(result.columns, { name = "doubled", data_type = "INTEGER" })
            for _, row in ipairs(result.rows) do
                table.insert(row, row[1] * 2)
            end
        end)
        "#,
    )
    .unwrap();

    let (mut core, _dir) = core_with_items().await;
    core.register_lua_plugin(plugin).unwrap();

    core.insert_into_editor("SELECT id, label FROM items ORDER BY id");
    core.execute_command("run");
    core.drain_run_updates().await;

    match core.result() {
        ResultState::Rows { columns, rows, .. } => {
            assert_eq!(columns.len(), 3);
            assert_eq!(columns[2].name, "doubled");
            assert_eq!(rows[0].0[2].render(), "2");
            assert_eq!(rows[2].0[2].render(), "6");
        }
        other => panic!("expected Rows, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registering_a_command_that_shadows_a_builtin_is_rejected() {
    // Plugin tries to claim ':run' — a built-in name. The parser would
    // always match the built-in first, so silently allowing this would
    // leave the user wondering why their override never fires.
    let plugin = LuaPlugin::from_script(
        "shadower",
        r#"
        narwhal.register_command("run", "hijack run", function() return "hi" end)
        "#,
    )
    .unwrap();

    let mut core = empty_core();
    let err = core.register_lua_plugin(plugin).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("shadows a built-in"),
        "unexpected error: {msg}"
    );
    // And the registry stays empty — the rejected plugin must not be
    // partially registered.
    assert!(core.plugins().catalogue().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transform_chain_continues_past_a_failing_plugin() {
    // Two plugins: the first transform errors, the second tags every
    // row with a synthetic '__seen' column. The second one must still
    // run — a broken first plugin shouldn't be able to suppress every
    // transform that follows.
    let broken = LuaPlugin::from_script(
        "broken",
        r#"
        narwhal.register_transform(function(_) error("nope") end)
        "#,
    )
    .unwrap();
    let tagger = LuaPlugin::from_script(
        "tagger",
        r#"
        narwhal.register_transform(function(result)
            table.insert(result.columns, { name = "__seen", data_type = "TEXT" })
            for _, row in ipairs(result.rows) do
                table.insert(row, "yes")
            end
        end)
        "#,
    )
    .unwrap();

    let (mut core, _dir) = core_with_items().await;
    core.register_lua_plugin(broken).unwrap();
    core.register_lua_plugin(tagger).unwrap();

    core.insert_into_editor("SELECT id, label FROM items ORDER BY id");
    core.execute_command("run");
    core.drain_run_updates().await;

    match core.result() {
        ResultState::Rows { columns, rows, .. } => {
            assert_eq!(columns.last().map(|c| c.name.as_str()), Some("__seen"));
            assert_eq!(rows.len(), 3);
            for row in rows {
                assert_eq!(row.0.last().map(|v| v.render()), Some("yes".into()));
            }
        }
        other => panic!("expected Rows, got {other:?}"),
    }
    let msg = core.status_message();
    assert!(msg.contains("nope"), "missing first-plugin error: {msg}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sql_run_during_open_transaction_is_refused() {
    // While a `:begin` transaction is open, the executor must refuse
    // sql_run — a fresh pool connection wouldn't see the pinned
    // transaction's writes, which would silently feed scripts wrong data.
    let plugin = LuaPlugin::from_script(
        "peek",
        r#"
        narwhal.register_command("peek", "count items", function(_)
            local r = narwhal.sql_run("SELECT COUNT(*) FROM items")
            return "items=" .. tostring(r.rows[1][1])
        end)
        "#,
    )
    .unwrap();

    let (mut core, _dir) = core_with_items().await;
    core.register_lua_plugin(plugin).unwrap();

    // Before :begin — works.
    core.execute_command("peek");
    assert_eq!(core.status_message(), "items=3");

    core.execute_command("begin");
    assert!(core.status_message().contains("transaction started"));

    // During :begin — refused.
    core.execute_command("peek");
    let msg = core.status_message();
    assert!(
        msg.contains("unavailable while a :begin transaction is open"),
        "expected refusal, got: {msg}"
    );

    // After :commit — works again.
    core.execute_command("commit");
    core.execute_command("peek");
    assert_eq!(core.status_message(), "items=3");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registering_two_commands_with_the_same_name_in_one_plugin_fails_at_load() {
    // Script-author error: same name twice. Should fail at
    // `LuaPlugin::from_script` with a useful message rather than
    // registering a half-broken plugin downstream.
    let outcome = LuaPlugin::from_script(
        "dup",
        r#"
        narwhal.register_command("foo", "v1", function() return "a" end)
        narwhal.register_command("foo", "v2", function() return "b" end)
        "#,
    );
    let err = match outcome {
        Ok(_) => panic!("expected duplicate-register to fail"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(
        msg.contains("already registered") && msg.contains("foo"),
        "unexpected error: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transform_failure_surfaces_in_status_but_keeps_rows() {
    let plugin = LuaPlugin::from_script(
        "broken",
        r#"
        narwhal.register_transform(function(result)
            error("transform exploded")
        end)
        "#,
    )
    .unwrap();

    let (mut core, _dir) = core_with_items().await;
    core.register_lua_plugin(plugin).unwrap();

    core.insert_into_editor("SELECT id, label FROM items ORDER BY id");
    core.execute_command("run");
    core.drain_run_updates().await;

    let msg = core.status_message();
    assert!(
        msg.contains("transform failed") || msg.contains("exploded"),
        "unexpected status: {msg}"
    );
    // Rows are still there even though the transform errored.
    match core.result() {
        ResultState::Rows { rows, .. } => assert_eq!(rows.len(), 3),
        other => panic!("expected Rows, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shipped_example_plugins_load_and_work() {
    // Walks examples/plugins/, registers every .lua file, and exercises
    // the commands that don't depend on a real connection. Catches
    // regressions where the script API drifts away from what the docs
    // ship to users.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let plugins_dir = manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/plugins");
    assert!(
        plugins_dir.is_dir(),
        "examples/plugins not found at {}",
        plugins_dir.display()
    );

    let (mut core, _dir) = core_with_items().await;
    let loaded = core.auto_load_plugins(&plugins_dir);
    assert!(loaded >= 4, "expected ≥4 plugins, got {loaded}");

    // :top exercises the snippet plugin (sql injection outcome).
    core.execute_command("top items");
    assert!(
        core.editor()
            .entire_text()
            .contains("SELECT * FROM items LIMIT 10"),
        "editor missing snippet, got: {:?}",
        core.editor().entire_text()
    );

    // :rc exercises the sql_run plugin against the active sqlite.
    core.execute_command("rc items");
    assert_eq!(core.status_message(), "items: 3 row(s)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sql_run_from_lua_hits_active_session() {
    // Script counts rows in 'items' via narwhal.sql_run and returns it.
    let plugin = LuaPlugin::from_script(
        "counter",
        r#"
        narwhal.register_command("howmany", "count items", function(_)
            local r = narwhal.sql_run("SELECT COUNT(*) FROM items")
            return "items=" .. tostring(r.rows[1][1])
        end)
        "#,
    )
    .unwrap();

    let (mut core, _dir) = core_with_items().await;
    core.register_lua_plugin(plugin).unwrap();

    core.execute_command("howmany");
    assert_eq!(core.status_message(), "items=3");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sql_run_without_active_session_reports_error() {
    // Same plugin but no open connection — the executor should refuse
    // and the Lua call surfaces as a plugin handler error.
    let plugin = LuaPlugin::from_script(
        "counter",
        r#"
        narwhal.register_command("go", "", function(_)
            narwhal.sql_run("SELECT 1")
            return "never"
        end)
        "#,
    )
    .unwrap();

    let mut core = empty_core();
    core.register_lua_plugin(plugin).unwrap();

    core.execute_command("go");
    let msg = core.status_message();
    assert!(
        msg.contains("plugin error") && msg.contains("no active connection"),
        "got: {msg}"
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
    core.register_lua_plugin(plugin).unwrap();

    core.execute_command("boom");
    let msg = core.status_message();
    assert!(
        msg.contains("plugin error") || msg.contains("intentional"),
        "got: {msg}"
    );
}
