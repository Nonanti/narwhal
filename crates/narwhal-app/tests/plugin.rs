//! Integration tests for the Lua-backed plugin pipeline inside `AppCore`.
//!
//! These tests build a real [`AppCore`] (no open connection — plugin
//! commands don't need one) and drive the `:` prompt through
//! [`AppCore::execute_command`]. The Lua scripts are written inline so
//! the test files stay self-contained.

use std::path::PathBuf;

use narwhal_app::DriverRegistry;
use narwhal_app::core::{AppCore, ResultState};
use narwhal_config::ConnectionsFile;
use narwhal_core::{ConnectionConfig, ConnectionParams};
use narwhal_plugin_lua::LuaPlugin;
use tempfile::TempDir;
use uuid::Uuid;

fn empty_core() -> AppCore {
    let registry = DriverRegistry::with_defaults();
    let connections = ConnectionsFile {
        schema_version: None,
        logical_relations: Vec::new(),
        connections: Vec::new(),
    };
    AppCore::new(registry, connections, None)
}

/// Build an `AppCore` wired to a freshly-seeded sqlite database with a
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
        schema_version: None,
        logical_relations: Vec::new(),
        connections: vec![ConnectionConfig {
            id: Uuid::nil(),
            name: "p".into(),
            driver: "sqlite".into(),
            params: ConnectionParams::with(|p| {
                p.path = Some(db_path.to_string_lossy().into_owned());
            }),
        }],
    };
    let mut core = AppCore::new(registry, connections, None);
    core.execute_command("open p").await;
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
    core.execute_command(&format!("plug-load {}", script_path.display()))
        .await;
    assert!(
        core.status_message().contains("loaded"),
        "expected load confirmation, got: {}",
        core.status_message()
    );

    // The new command 'shout' should now be reachable from the `:` prompt.
    core.execute_command("shout hello berkant").await;
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
    core.execute_command(&format!("plug-load {}", script_path.display()))
        .await;
    core.execute_command("plug-list").await;
    let msg = core.status_message();
    assert!(msg.contains("alpha"), "missing alpha: {msg}");
    assert!(msg.contains("beta"), "missing beta: {msg}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plug_list_with_no_plugins_hints_at_plug_load() {
    let mut core = empty_core();
    core.execute_command("plug-list").await;
    assert!(core.status_message().contains("plug-load"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plug_load_with_bad_script_reports_error_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = write_script(&dir, "broken.lua", "this is not valid lua )");

    let mut core = empty_core();
    core.execute_command(&format!("plug-load {}", script_path.display()))
        .await;
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

    core.execute_command("count users").await;
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

    core.execute_command("a").await;
    assert_eq!(core.status_message(), "A");
    core.execute_command("b").await;
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
    core.execute_command("good").await;
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
    core.execute_command("this-name-does-not-exist arg").await;
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

    core.insert_into_editor("SELECT id, label FROM items ORDER BY id")
        .await;
    core.execute_command("run").await;
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

    core.insert_into_editor("SELECT id, label FROM items ORDER BY id")
        .await;
    core.execute_command("run").await;
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

    core.insert_into_editor("SELECT id, label FROM items ORDER BY id")
        .await;
    core.execute_command("run").await;
    core.drain_run_updates().await;

    match core.result() {
        ResultState::Rows { columns, rows, .. } => {
            assert_eq!(columns.last().map(|c| c.name.as_str()), Some("__seen"));
            assert_eq!(rows.len(), 3);
            for row in rows {
                assert_eq!(
                    row.0.last().map(narwhal_core::Value::render),
                    Some("yes".into())
                );
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
    core.execute_command("peek").await;
    assert_eq!(core.status_message(), "items=3");

    core.execute_command("begin").await;
    assert!(core.status_message().contains("transaction started"));

    // During :begin — refused.
    core.execute_command("peek").await;
    let msg = core.status_message();
    assert!(
        msg.contains("unavailable while a :begin transaction is open"),
        "expected refusal, got: {msg}"
    );

    // After :commit — works again.
    core.execute_command("commit").await;
    core.execute_command("peek").await;
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

    core.insert_into_editor("SELECT id, label FROM items ORDER BY id")
        .await;
    core.execute_command("run").await;
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
    // v2.2.0: the default sandbox is Restricted, but csv_export.lua
    // needs `io.open`. Load each example file manually so we can give
    // csv_export the Permissive sandbox while keeping every other
    // example under the safe default — this exercises the new
    // from_path_with_sandbox opt-in API end-to-end.
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&plugins_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("lua"))
        .collect();
    entries.sort();
    let mut loaded = 0;
    for path in &entries {
        let needs_permissive = path.file_name().and_then(|s| s.to_str()) == Some("csv_export.lua");
        let sandbox = if needs_permissive {
            narwhal_plugin_lua::LuaSandbox::Permissive
        } else {
            narwhal_plugin_lua::LuaSandbox::default()
        };
        let plugin = narwhal_plugin_lua::LuaPlugin::from_path_with_sandbox(path, sandbox)
            .unwrap_or_else(|e| panic!("load {}: {e}", path.display()));
        core.register_lua_plugin(plugin)
            .unwrap_or_else(|e| panic!("register {}: {e}", path.display()));
        loaded += 1;
    }
    assert!(loaded >= 5, "expected ≥5 plugins, got {loaded}");

    // :top exercises the snippet plugin (sql injection outcome).
    core.execute_command("top items").await;
    assert!(
        core.editor()
            .entire_text()
            .contains("SELECT * FROM items LIMIT 10"),
        "editor missing snippet, got: {:?}",
        core.editor().entire_text()
    );

    // :rc exercises the sql_run plugin against the active sqlite.
    core.execute_command("rc items").await;
    assert_eq!(core.status_message(), "items: 3 row(s)");

    // :csv-export was loaded above under Permissive.
    // Per-test tempdir keeps parallel runs of this suite from racing on
    // a single fixed /tmp path.
    let csv_dir = tempfile::tempdir().unwrap();
    let csv_path = csv_dir.path().join("items.csv");
    let csv_cmd = format!("csv-export items {}", csv_path.display());
    core.execute_command(&csv_cmd).await;
    let csv_status = core.status_message().to_owned();
    assert!(
        csv_status.contains("wrote 3 row(s) to"),
        "expected csv-export success, got: {csv_status}"
    );
    // Verify the file contents: header + 3 data rows.
    let contents = std::fs::read_to_string(&csv_path).unwrap_or_default();
    let lines: Vec<&str> = contents.trim().lines().collect();
    assert_eq!(
        lines.len(),
        4,
        "expected 4 lines (header + 3 rows), got {}",
        lines.len()
    );
    assert!(
        lines[0].contains("id"),
        "header should contain 'id': {}",
        lines[0]
    );
    assert!(
        lines[0].contains("label"),
        "header should contain 'label': {}",
        lines[0]
    );

    // :explain-cost exercises the editor-wrapping plugin.
    // Clear the editor first, then seed it with a statement.
    core.execute_command("clear").await;
    core.insert_into_editor("SELECT * FROM items").await;
    core.execute_command("explain-cost").await;
    let editor_after = core.editor().entire_text();
    assert!(
        editor_after.contains("EXPLAIN ANALYZE"),
        "editor should contain EXPLAIN ANALYZE, got: {editor_after:?}"
    );
    assert!(
        editor_after.contains("SELECT * FROM items"),
        "editor should still contain the original statement, got: {editor_after:?}"
    );
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

    core.execute_command("howmany").await;
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

    core.execute_command("go").await;
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

    core.execute_command("boom").await;
    let msg = core.status_message();
    assert!(
        msg.contains("plugin error") || msg.contains("intentional"),
        "got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn help_with_builtin_arg_describes_it() {
    let mut core = empty_core();

    // :help open should print a description containing "open".
    core.execute_command("help open").await;
    let msg = core.status_message();
    assert!(
        msg.contains("open"),
        "expected :help open to mention 'open', got: {msg}"
    );

    // :help with no arg still shows the one-liner.
    core.execute_command("help").await;
    let msg = core.status_message();
    assert!(
        msg.contains("quit"),
        "expected :help to list commands, got: {msg}"
    );

    // :help with an unknown name.
    core.execute_command("help nonexistent-cmd").await;
    let msg = core.status_message();
    assert!(
        msg.contains("unknown command"),
        "expected unknown-command message, got: {msg}"
    );

    // :help alias resolves correctly.
    core.execute_command("help o").await;
    let msg = core.status_message();
    assert!(
        msg.contains("open"),
        "expected :help o to resolve to open description, got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn help_with_plugin_arg_describes_it() {
    let dir = tempfile::tempdir().unwrap();
    let script_path = write_script(
        &dir,
        "row_count.lua",
        r#"
        narwhal.register_command("rc", "row count for <table>", function(arg)
            local raw = arg:match("^%s*(.-)%s*$")
            return raw .. ": ok"
        end)
        "#,
    );

    let mut core = empty_core();
    core.execute_command(&format!("plug-load {}", script_path.display()))
        .await;
    assert!(core.status_message().contains("loaded"));

    // :help rc should surface the plugin's description.
    core.execute_command("help rc").await;
    let msg = core.status_message();
    assert!(
        msg.contains("row count"),
        "expected :help rc to contain plugin description, got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explain_cost_wraps_editor_buffer() {
    let plugin = LuaPlugin::from_script(
        "explain_cost",
        r#"
        narwhal.register_command("explain-cost", "wrap editor buffer in EXPLAIN ANALYZE", function(_)
            local text = narwhal.editor_text or ""
            local trimmed = text:match("^%s*(.-)%s*$") or ""
            if trimmed == "" then
                return "explain-cost: editor is empty; type a statement first"
            end
            return {
                sql = "EXPLAIN ANALYZE " .. trimmed .. "\n",
                append = false,
            }
        end)
        "#,
    )
    .unwrap();

    let mut core = empty_core();
    core.register_lua_plugin(plugin).unwrap();

    // Empty buffer — should get a hint message.
    core.execute_command("explain-cost").await;
    let msg = core.status_message();
    assert!(
        msg.contains("editor is empty"),
        "expected empty-buffer hint, got: {msg}"
    );

    // Seed the editor with a statement, then wrap it.
    core.insert_into_editor("SELECT 1").await;
    core.execute_command("explain-cost").await;
    let editor = core.editor().entire_text();
    assert!(
        editor.contains("EXPLAIN ANALYZE"),
        "editor should contain EXPLAIN ANALYZE, got: {editor:?}"
    );
    assert!(
        editor.contains("SELECT 1"),
        "editor should still contain the original statement, got: {editor:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_timeout_uses_resolved_plugin_name_and_hints_at_set_timeout() {
    // H20 regression: the timeout error message must contain the
    // *resolved* plugin name (not a re-lookup that could resolve
    // differently) and an actionable hint mentioning
    // `narwhal.set_timeout`.
    //
    // v2.2.0: the default sandbox is Restricted (no `os`), so this
    // test uses a pure-Lua busy loop rather than `os.clock()`. The
    // 0.05 s budget is small enough that the iteration count below
    // will trip the hook well before completing.
    let plugin = LuaPlugin::from_script(
        "slowpoke",
        r#"
        narwhal.set_timeout(0.05)
        narwhal.register_command("slow", "intentionally slow", function(_)
            local x = 0
            for i = 1, 1e10 do x = x + 1 end
            return "never"
        end)
        "#,
    )
    .unwrap();

    let mut core = empty_core();
    core.register_lua_plugin(plugin).unwrap();

    core.execute_command("slow arg").await;
    let msg = core.status_message().to_owned();
    // The message must reference the plugin name, not the command.
    assert!(
        msg.contains("slowpoke"),
        "expected plugin name 'slowpoke' in timeout message, got: {msg}"
    );
    // The message must mention the escape hatch.
    assert!(
        msg.contains("narwhal.set_timeout"),
        "expected actionable hint in timeout message, got: {msg}"
    );
    assert!(
        msg.contains("execution timeout"),
        "expected 'execution timeout' in message, got: {msg}"
    );
}
