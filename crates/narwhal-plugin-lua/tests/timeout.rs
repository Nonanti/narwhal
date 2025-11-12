//! Tests for the Lua plugin execution timeout mechanism.
//!
//! A plugin with `while true do end` must be interrupted after the
//! configured budget expires. The default budget is 5 seconds, but
//! tests use 100 ms via `narwhal.set_timeout(0.1)` to keep CI fast.

use narwhal_plugin::{CommandContext, CommandOutcome, Plugin, PluginError};
use narwhal_plugin_lua::LuaPlugin;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn infinite_loop_times_out() {
    let plugin = LuaPlugin::from_script(
        "hanging",
        r#"
        narwhal.set_timeout(0.1)
        narwhal.register_command("hang", "hang forever", function()
            while true do end
        end)
        "#,
    )
    .unwrap();

    let err = plugin
        .dispatch("hang", CommandContext::default())
        .await
        .unwrap_err();

    match err {
        PluginError::Timeout { elapsed_secs } => {
            // Should have timed out after roughly 100 ms.
            assert!(
                elapsed_secs >= 0.1,
                "expected ≥ 0.1s, got {elapsed_secs:.3}s"
            );
            // And not taken much longer (CI slack: up to ~2 s).
            assert!(
                elapsed_secs < 2.0,
                "expected < 2.0s, got {elapsed_secs:.3}s"
            );
        }
        other => panic!("expected Timeout, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_timeout_extends_budget() {
    // The plugin requests a 500 ms budget, then runs a 200 ms busy
    // loop — well within the limit. Should succeed without a timeout.
    let plugin = LuaPlugin::from_script(
        "slow-ok",
        r#"
        narwhal.set_timeout(0.5)
        narwhal.register_command("slow", "slow but within budget", function()
            local deadline = os.clock() + 0.2
            while os.clock() < deadline do end
            return "ok"
        end)
        "#,
    )
    .unwrap();

    let outcome = plugin
        .dispatch("slow", CommandContext::default())
        .await
        .unwrap();

    match outcome {
        CommandOutcome::Status { message } => assert_eq!(message, "ok"),
        other => panic!("expected Status, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn normal_plugin_unaffected() {
    // No set_timeout call — default 5 s budget applies, but the
    // handler returns instantly so the hook never trips.
    let plugin = LuaPlugin::from_script(
        "fast",
        r#"
        narwhal.register_command("fast", "fast", function()
            return "quick"
        end)
        "#,
    )
    .unwrap();

    let outcome = plugin
        .dispatch("fast", CommandContext::default())
        .await
        .unwrap();

    match outcome {
        CommandOutcome::Status { message } => assert_eq!(message, "quick"),
        other => panic!("expected Status, got {other:?}"),
    }
}
