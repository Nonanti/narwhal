//! Tiny example plugin: logs a line for every lifecycle event narwhal
//! sends. Demonstrates the canonical control flow:
//!
//! 1. `init` returns the plugin's identity to the host.
//! 2. `handle-command` answers `:` prompt invocations declared in
//!    `plugin.toml` (none in this example).
//! 3. `on-event` reacts to host lifecycle events by emitting a log
//!    line through `host.log`.

#![no_std]

wit_bindgen::generate!({
    path: "../../wit",
    world: "narwhal-plugin",
});

extern crate alloc;

use alloc::format;
use alloc::string::String;

use exports::narwhal::plugin::plugin::{
    CommandInput, CommandOutcome, Event, Guest, PluginInfo,
};
use narwhal::plugin::host;

struct HelloWorld;

impl Guest for HelloWorld {
    fn init() -> Result<PluginInfo, String> {
        host::log("info", "hello-world plugin initialising");
        Ok(PluginInfo {
            name: String::from("hello-world"),
            version: String::from("0.1.0"),
            api_version: 1,
        })
    }

    fn handle_command(_input: CommandInput) -> Result<CommandOutcome, String> {
        // This example doesn't expose any commands in `plugin.toml`,
        // but a real plugin would dispatch by `_input.name` here.
        Ok(CommandOutcome::Silent)
    }

    fn on_event(event: Event) -> Result<(), String> {
        let line = match event {
            Event::ConnectionOpened(name) => format!("connection opened: {name}"),
            Event::ConnectionClosed(name) => format!("connection closed: {name}"),
            Event::QueryStarted(sql) => format!("query started: {sql}"),
            Event::QueryFinished(summary) => format!(
                "query finished: ok={} rows={} elapsed_ms={}",
                summary.ok, summary.rows, summary.elapsed_ms
            ),
            Event::EditorBufferChanged(snap) => format!(
                "buffer changed: line={} col={} bytes={}",
                snap.cursor_line,
                snap.cursor_col,
                snap.text.len()
            ),
        };
        host::log("info", &line);
        Ok(())
    }
}

export!(HelloWorld);
