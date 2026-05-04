//! Tests for the Lua plugin execution timeout mechanism.
//!
//! A plugin with `while true do end` must be interrupted after the
//! configured budget expires. The default budget is 5 seconds, but
//! tests use 100 ms via `narwhal.set_timeout(0.1)` to keep CI fast.

use narwhal_plugin::{
    ColumnHeader, CommandContext, CommandOutcome, Plugin, PluginError, QueryResult, Row, Value,
};
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
    // The plugin requests a 500 ms budget, then runs a ~200 ms busy
    // loop — well within the limit. Should succeed without a timeout.
    //
    // v2.2.0: the default sandbox is Restricted (no `os`), so we use
    // a fixed-iteration counter loop instead of `os.clock()`. The
    // exact wall-clock duration is unimportant; we only need the
    // loop body to take well under 500 ms.
    let plugin = LuaPlugin::from_script(
        "slow-ok",
        r#"
        narwhal.set_timeout(0.5)
        narwhal.register_command("slow", "slow but within budget", function()
            -- ~1e6 iterations of a no-op stay well under 500 ms on
            -- any tier-1 host. The hook fires every ~256 instructions
            -- so the loop is also covered by the timeout machinery.
            local x = 0
            for i = 1, 1000000 do x = x + 1 end
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_timeout_large_value_no_panic() {
    // Values like 1e20, 1e30, or f64::MAX must not panic inside
    // Duration::from_secs_f64. They should be clamped to Duration::MAX
    // (timeout disabled), and the handler should run normally.
    for val in ["1e20", "1e30", "1e308"] {
        let script = format!(
            r#"
            narwhal.set_timeout({val})
            narwhal.register_command("ok", "ok", function()
                return "survived"
            end)
            "#
        );
        let plugin = LuaPlugin::from_script("big-timeout", &script).unwrap();
        let outcome = plugin
            .dispatch("ok", CommandContext::default())
            .await
            .unwrap();
        match outcome {
            CommandOutcome::Status { message } => assert_eq!(message, "survived"),
            other => panic!("expected Status for {val}, got {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_timeout_nan_disables() {
    // NaN should be treated as "disable timeout" (Duration::MAX),
    // not panic.
    let plugin = LuaPlugin::from_script(
        "nan-timeout",
        r#"
        narwhal.set_timeout(0/0)
        narwhal.register_command("ok", "ok", function()
            return "nan-ok"
        end)
        "#,
    )
    .unwrap();
    let outcome = plugin
        .dispatch("ok", CommandContext::default())
        .await
        .unwrap();
    match outcome {
        CommandOutcome::Status { message } => assert_eq!(message, "nan-ok"),
        other => panic!("expected Status, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_timeout_negative_disables() {
    // Negative values should be treated as "disable timeout".
    let plugin = LuaPlugin::from_script(
        "neg-timeout",
        r#"
        narwhal.set_timeout(-5)
        narwhal.register_command("ok", "ok", function()
            return "neg-ok"
        end)
        "#,
    )
    .unwrap();
    let outcome = plugin
        .dispatch("ok", CommandContext::default())
        .await
        .unwrap();
    match outcome {
        CommandOutcome::Status { message } => assert_eq!(message, "neg-ok"),
        other => panic!("expected Status, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transform_infinite_loop_times_out() {
    // A transform handler that loops forever must be interrupted by
    // the timeout mechanism, just like command handlers are.
    let plugin = LuaPlugin::from_script(
        "hanging-transform",
        r"
        narwhal.set_timeout(0.1)
        narwhal.register_transform(function(result)
            while true do end
        end)
        ",
    )
    .unwrap();

    let mut result = QueryResult {
        columns: vec![ColumnHeader {
            name: "x".into(),
            data_type: "INTEGER".into(),
        }],
        rows: vec![Row(vec![Value::Int(1)])],
        rows_affected: None,
        elapsed_ms: 1,
    };

    let err = plugin.transform_result(&mut result).await.unwrap_err();
    match err {
        PluginError::Timeout { elapsed_secs } => {
            assert!(
                elapsed_secs >= 0.1,
                "expected ≥ 0.1s, got {elapsed_secs:.3}s"
            );
            assert!(
                elapsed_secs < 2.0,
                "expected < 2.0s, got {elapsed_secs:.3}s"
            );
        }
        other => panic!("expected Timeout, got {other:?}"),
    }

    // Original data should be preserved despite the error.
    assert_eq!(result.rows.len(), 1);
}
