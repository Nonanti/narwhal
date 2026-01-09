//! H14 — `outcome_from_lua` contract regression tests.
//!
//! Before the fix:
//!   * `return true` fell into the unsupported-value branch with a
//!     confusing "unsupported return value: boolean" message, even
//!     though the natural reading is "command handled, no UI".
//!   * `{ sql = 42 }` (or any non-string `sql`) was silently rerouted
//!     into the "missing field" branch, hiding the real type mismatch
//!     from the plugin author.

use narwhal_plugin::{CommandContext, CommandOutcome, Plugin};
use narwhal_plugin_lua::LuaPlugin;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn return_true_is_silent() {
    let plugin = LuaPlugin::from_script(
        "p",
        r#"
        narwhal.register_command("ok", "", function() return true end)
        "#,
    )
    .unwrap();

    let outcome = plugin
        .dispatch("ok", CommandContext::default())
        .await
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::Silent));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sql_field_must_be_string() {
    let plugin = LuaPlugin::from_script(
        "p",
        r#"
        narwhal.register_command("bad", "", function()
            return { sql = 42 }
        end)
        "#,
    )
    .unwrap();

    let err = plugin
        .dispatch("bad", CommandContext::default())
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("'sql' must be a string"),
        "expected explicit type error, got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_field_must_be_string() {
    let plugin = LuaPlugin::from_script(
        "p",
        r#"
        narwhal.register_command("bad", "", function()
            return { status = {} }
        end)
        "#,
    )
    .unwrap();

    let err = plugin
        .dispatch("bad", CommandContext::default())
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("'status' must be a string"),
        "expected explicit type error, got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_table_errors_clearly() {
    let plugin = LuaPlugin::from_script(
        "p",
        r#"
        narwhal.register_command("bad", "", function() return {} end)
        "#,
    )
    .unwrap();

    let err = plugin
        .dispatch("bad", CommandContext::default())
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("'sql'") || msg.contains("'status'"),
        "expected hint about sql/status, got: {msg}"
    );
}
