//! Plugin system for narwhal.
//!
//! A plugin extends narwhal with one or more *capabilities*:
//!
//! - **Commands** — string-keyed handlers invoked from the `:` prompt.
//!   The host passes a [`CommandContext`] (which today exposes only the
//!   raw argument string) and the plugin returns a [`CommandOutcome`].
//! - **Result transforms** — post-processing hooks that can rewrite the
//!   rows or annotations of a query result before it reaches the UI.
//!
//! The trait is async-friendly and object-safe so the host can keep a
//! `Vec<Arc<dyn Plugin>>` and route the dispatch by name at runtime.
//! Concrete plugin runtimes (Lua, WASM, native Rust) live in their own
//! crates and only need to depend on this one.
//!
//! ### Why a separate crate?
//!
//! Plugin runtimes (Lua via `mlua`, WASM via `wasmtime`) drag in chunky
//! dependencies. Keeping the trait in a lean crate means downstream
//! consumers — and the rest of `narwhal-app` — can compile against the
//! abstraction without paying for any specific runtime.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use narwhal_core::{ColumnHeader, QueryResult, Row, Value};

/// Errors that surface from plugin invocations.
#[derive(Debug, Error)]
pub enum PluginError {
    /// A handler returned a structured failure (with a user-facing message).
    #[error("{0}")]
    Handler(String),
    /// The plugin's underlying runtime (Lua VM, WASM module, …) failed.
    #[error("plugin runtime error: {0}")]
    Runtime(String),
    /// The dispatched name isn't bound by any plugin.
    #[error("unknown plugin command: {0}")]
    Unknown(String),
}

pub type PluginResult<T> = std::result::Result<T, PluginError>;

/// Single command exposed by a plugin. The combination of `(plugin name,
/// command name)` must be unique inside a [`PluginRegistry`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandDescriptor {
    /// Identifier typed at the `:` prompt (e.g. `format-table`).
    pub name: String,
    /// One-line human-readable description shown in `:help` listings.
    pub description: String,
}

/// What a command handler may report to the host. Side effects beyond
/// "show this status message" are intentionally not in scope yet — they
/// would force a tighter coupling with the AppCore than we want here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandOutcome {
    /// Update the status bar with `message`.
    Status { message: String },
    /// Inject `sql` into the editor (replacing or appending — the host
    /// decides based on the [`append`] flag).
    InsertSql { sql: String, append: bool },
    /// Quietly succeed without surfacing anything.
    Silent,
}

/// Context handed to a command handler. Today it only carries the raw
/// argument string from the prompt; future fields can be added without
/// breaking plugins because every consumer constructs it through the
/// public API (the struct is `non_exhaustive`).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CommandContext {
    pub argument: String,
}

impl CommandContext {
    pub fn new(argument: impl Into<String>) -> Self {
        Self {
            argument: argument.into(),
        }
    }
}

/// Bridge that lets a plugin run SQL against narwhal's active
/// connection. The host implements this on top of its connection pool
/// and injects an `Arc<dyn SqlExecutor>` into each plugin runtime that
/// supports user-driven SQL execution (today only [`narwhal-plugin-lua`]).
///
/// The trait is async because the underlying driver API is async. Plugin
/// runtimes that need a sync surface (e.g. Lua) bridge the call with
/// `tokio::task::block_in_place + Handle::current().block_on(...)`.
#[async_trait]
pub trait SqlExecutor: Send + Sync {
    /// Execute `sql` against whatever connection the host considers
    /// active. Returns the materialised [`QueryResult`] or a runtime
    /// error suitable for surfacing to the script.
    async fn run(&self, sql: &str) -> PluginResult<QueryResult>;
}

/// Public surface every plugin runtime implements. The trait is
/// intentionally tiny — additions go through default methods so older
/// plugins continue to compile.
#[async_trait]
pub trait Plugin: Send + Sync {
    /// Stable identifier (e.g. `"lua"`, `"format-csv"`). Used as a
    /// namespace inside [`PluginRegistry`].
    fn name(&self) -> &str;

    /// Commands the plugin exposes. The default implementation returns
    /// an empty slice for plugins that only register transforms.
    fn commands(&self) -> Vec<CommandDescriptor> {
        Vec::new()
    }

    /// Dispatch a command by name. The default implementation reports
    /// the name as unknown; plugins that override [`commands`] also
    /// override this.
    async fn dispatch(&self, name: &str, ctx: CommandContext) -> PluginResult<CommandOutcome> {
        let _ = ctx;
        Err(PluginError::Unknown(name.to_owned()))
    }

    /// Optional post-processing hook called after every successful query.
    /// Plugins that don't need it use the default no-op. The default
    /// receives `&mut QueryResult` so transforms can edit in place
    /// without allocating.
    async fn transform_result(&self, result: &mut QueryResult) -> PluginResult<()> {
        let _ = result;
        Ok(())
    }
}

/// Concrete plugin registry. Owned by the host (`AppCore`) and consulted
/// when the `:` prompt produces an unknown built-in command.
///
/// The registry is keyed by command name (not by plugin name) so dispatch
/// is O(1). The plugin that owns a given command is tracked alongside so
/// the host can show provenance in `:help`.
#[derive(Default, Clone)]
pub struct PluginRegistry {
    plugins: Vec<Arc<dyn Plugin>>,
    by_command: BTreeMap<String, Arc<dyn Plugin>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `plugin` and index its commands. Returns the number of
    /// commands that were *new*; duplicates raise a [`PluginError`].
    pub fn register<P: Plugin + 'static>(&mut self, plugin: P) -> PluginResult<usize> {
        let plugin: Arc<dyn Plugin> = Arc::new(plugin);
        let descriptors = plugin.commands();
        for d in &descriptors {
            if self.by_command.contains_key(&d.name) {
                return Err(PluginError::Handler(format!(
                    "command '{}' already registered",
                    d.name
                )));
            }
        }
        let count = descriptors.len();
        for d in descriptors {
            self.by_command.insert(d.name, plugin.clone());
        }
        self.plugins.push(plugin);
        Ok(count)
    }

    /// Every plugin currently loaded, in registration order.
    pub fn plugins(&self) -> &[Arc<dyn Plugin>] {
        &self.plugins
    }

    /// Look up the plugin that owns `command`.
    pub fn plugin_for(&self, command: &str) -> Option<Arc<dyn Plugin>> {
        self.by_command.get(command).cloned()
    }

    /// Dispatch `command` against whichever plugin owns it.
    pub async fn dispatch(
        &self,
        command: &str,
        ctx: CommandContext,
    ) -> PluginResult<CommandOutcome> {
        let plugin = self
            .plugin_for(command)
            .ok_or_else(|| PluginError::Unknown(command.to_owned()))?;
        plugin.dispatch(command, ctx).await
    }

    /// Run every plugin's [`Plugin::transform_result`] hook in order.
    /// A failure aborts the chain so the user sees the first error
    /// rather than silently dropping subsequent transforms.
    pub async fn transform_result(&self, result: &mut QueryResult) -> PluginResult<()> {
        for plugin in &self.plugins {
            plugin.transform_result(result).await?;
        }
        Ok(())
    }

    /// Flattened command catalogue. Useful for `:help`.
    pub fn catalogue(&self) -> Vec<(String, CommandDescriptor)> {
        self.plugins
            .iter()
            .flat_map(|p| {
                let plugin_name = p.name().to_owned();
                p.commands()
                    .into_iter()
                    .map(move |d| (plugin_name.clone(), d))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal in-tree plugin used by the unit tests. It exposes a single
    /// "echo" command that returns the argument back as a status message.
    struct EchoPlugin;

    #[async_trait]
    impl Plugin for EchoPlugin {
        fn name(&self) -> &str {
            "echo"
        }

        fn commands(&self) -> Vec<CommandDescriptor> {
            vec![CommandDescriptor {
                name: "echo".into(),
                description: "echo the argument back to the status bar".into(),
            }]
        }

        async fn dispatch(&self, name: &str, ctx: CommandContext) -> PluginResult<CommandOutcome> {
            assert_eq!(name, "echo");
            Ok(CommandOutcome::Status {
                message: ctx.argument,
            })
        }
    }

    /// A trivial transform that appends a synthetic '__row_count' column
    /// to every result. Lets us exercise the result-transform path.
    struct CounterPlugin;

    #[async_trait]
    impl Plugin for CounterPlugin {
        fn name(&self) -> &str {
            "counter"
        }

        async fn transform_result(&self, result: &mut QueryResult) -> PluginResult<()> {
            result.columns.push(narwhal_core::ColumnHeader {
                name: "__row_count".into(),
                data_type: "BIGINT".into(),
            });
            let total = result.rows.len() as i64;
            for row in &mut result.rows {
                row.0.push(Value::Int(total));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn dispatch_routes_to_owning_plugin() {
        let mut registry = PluginRegistry::new();
        registry.register(EchoPlugin).unwrap();
        let outcome = registry
            .dispatch("echo", CommandContext::new("hi"))
            .await
            .unwrap();
        match outcome {
            CommandOutcome::Status { message } => assert_eq!(message, "hi"),
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn duplicate_command_is_rejected() {
        let mut registry = PluginRegistry::new();
        registry.register(EchoPlugin).unwrap();
        let err = registry.register(EchoPlugin).unwrap_err();
        assert!(matches!(err, PluginError::Handler(_)));
    }

    #[tokio::test]
    async fn unknown_command_is_reported() {
        let registry = PluginRegistry::new();
        let err = registry
            .dispatch("nope", CommandContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, PluginError::Unknown(_)));
    }

    #[tokio::test]
    async fn transform_chain_runs_in_registration_order() {
        let mut registry = PluginRegistry::new();
        registry.register(CounterPlugin).unwrap();
        let mut result = QueryResult {
            columns: vec![narwhal_core::ColumnHeader {
                name: "x".into(),
                data_type: "INT".into(),
            }],
            rows: vec![
                narwhal_core::Row(vec![Value::Int(1)]),
                narwhal_core::Row(vec![Value::Int(2)]),
            ],
            rows_affected: None,
            elapsed_ms: 0,
        };
        registry.transform_result(&mut result).await.unwrap();
        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.columns[1].name, "__row_count");
        assert_eq!(result.rows[0].0[1].render(), "2");
        assert_eq!(result.rows[1].0[1].render(), "2");
    }

    #[tokio::test]
    async fn catalogue_lists_every_command_with_its_plugin() {
        let mut registry = PluginRegistry::new();
        registry.register(EchoPlugin).unwrap();
        let cat = registry.catalogue();
        assert_eq!(cat.len(), 1);
        assert_eq!(cat[0].0, "echo");
        assert_eq!(cat[0].1.name, "echo");
    }
}
