//! Plugin lifecycle and dispatch extracted from `core.rs` (L21).
//!
//! Wraps the `:plug-load`, auto-load and registry-mutation entry points
//! plus the synchronous bridge from a `:`-prompt command to the async
//! [`PluginRegistry::dispatch`].
use std::sync::Arc;

use narwhal_plugin::{
    CommandContext as PluginCommandContext, CommandOutcome as PluginCommandOutcome, Plugin,
    PluginError, PluginRegistry, PluginResult, SqlExecutor,
};
use narwhal_plugin_lua::LuaPlugin;

use super::plugin_executor::AppPluginExecutor;
use super::AppCore;

impl AppCore {
    /// Read-only handle to the plugin registry, useful for tests.
    /// The `Arc` derefs transparently so callers can use `&PluginRegistry`
    /// methods without caring about the indirection.
    pub fn plugins(&self) -> &PluginRegistry {
        &self.plugins
    }

    /// Mutable handle so callers (binary or tests) can register plugins
    /// without going through the `:plug-load` command path.
    /// Uses `Arc::make_mut` so the clone-on-write only materialises when
    /// the caller actually mutates — dispatch paths that merely read pay
    /// a single ref-count bump.
    pub fn plugins_mut(&mut self) -> &mut PluginRegistry {
        Arc::make_mut(&mut self.plugins)
    }

    /// Register a freshly-built [`LuaPlugin`], wiring it into the SQL
    /// executor first so `narwhal.sql_run` works from inside the script.
    /// All host-driven registration paths (`:plug-load`,
    /// `auto_load_plugins`, integration tests) funnel through this so
    /// the executor injection is impossible to forget.
    pub fn register_lua_plugin(&mut self, plugin: LuaPlugin) -> PluginResult<usize> {
        let executor: Arc<dyn SqlExecutor> = Arc::new(AppPluginExecutor {
            state: self.plugin_state.clone(),
        });
        plugin.install_executor(executor)?;
        Arc::make_mut(&mut self.plugins).register(plugin)
    }

    /// Scan `dir` for top-level `*.lua` files and register each as a
    /// plugin. Returns the number of plugins that loaded successfully.
    /// Failures are accumulated into the status bar so the user notices
    /// at start-up; the rest of the directory keeps loading.
    ///
    /// Missing or unreadable directories are not an error — narwhal runs
    /// fine without any plugins.
    pub fn auto_load_plugins(&mut self, dir: &std::path::Path) -> usize {
        let entries = match std::fs::read_dir(dir) {
            Ok(it) => it,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return 0,
            Err(err) => {
                // Permission denied, ENOTDIR, etc. — the user almost
                // certainly wants to know; running without plugins is
                // also a valid choice, so we just warn rather than
                // abort startup.
                self.process.plugin_warning = Some(format!(
                    "plugin auto-load: cannot read {}: {err}",
                    dir.display()
                ));
                return 0;
            }
        };
        let mut paths: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.eq_ignore_ascii_case("lua"))
            })
            .collect();
        // Deterministic order so the registry index is reproducible.
        paths.sort();

        let mut loaded = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for path in &paths {
            match LuaPlugin::from_path(path) {
                Ok(plugin) => match self.register_lua_plugin(plugin) {
                    Ok(_) => loaded += 1,
                    Err(e) => failures.push(format!("{}: {e}", path.display())),
                },
                Err(e) => failures.push(format!("{}: {e}", path.display())),
            }
        }

        if !failures.is_empty() {
            // Surface failures via the plugin_warning slot so they survive
            // the next status message rewrite.
            self.process.plugin_warning = Some(format!(
                "{} plugin(s) failed to load: {}",
                failures.len(),
                failures.join("; ")
            ));
        }
        if loaded > 0 {
            self.status.message = format!("auto-loaded {loaded} plugin(s) from {}", dir.display());
        }
        loaded
    }

    pub(super) fn load_plugin(&mut self, path: &str) {
        let plugin = match LuaPlugin::from_path(path) {
            Ok(p) => p,
            Err(e) => {
                self.status.message = format!("plug-load failed: {e}");
                return;
            }
        };
        let name = plugin.name().to_owned();
        let cmd_count = plugin.commands().len();
        match self.register_lua_plugin(plugin) {
            Ok(_) => {
                self.status.message = format!("plugin '{name}' loaded ({cmd_count} command(s))");
            }
            Err(e) => {
                self.status.message = format!("plug-load failed: {e}");
            }
        }
    }

    pub(super) fn list_plugins(&mut self) {
        let catalogue = self.plugins.catalogue();
        if catalogue.is_empty() {
            self.status.message = "no plugins loaded; use :plug-load <file.lua>".into();
            return;
        }
        let summary = catalogue
            .iter()
            .map(|(plugin, cmd)| format!("{}:{} — {}", plugin, cmd.name, cmd.description))
            .collect::<Vec<_>>()
            .join(" · ");
        self.status.message = summary;
    }

    pub(super) fn dispatch_plugin(&mut self, command: &str, argument: &str) {
        let editor_text = self.tabs[self.active_tab].editor.entire_text();
        let ctx = PluginCommandContext::new(argument).with_editor_text(&editor_text);
        // Resolve the owning plugin name *before* dispatch so the timeout
        // handler reports the correct plugin even if two plugins share
        // the same command head (H20).
        let plugin_name = self
            .plugins
            .plugin_for(command)
            .map_or_else(|| command.to_owned(), |p| p.name().to_owned());
        let plugins = Arc::clone(&self.plugins);
        let command_owned = command.to_owned();
        // Sprint 9 (H7) deferred: an earlier attempt routed plugin
        // dispatch through the meta channel, but 11 integration tests
        // read `core.status_message()` synchronously right after
        // `core.execute_command("<plugin>")` — making the outcome
        // async requires either adding `core.drain_meta_updates()`
        // calls to ~33 test sites or building a synchronous waiting
        // shim that re-introduces the block. The trade-off was judged
        // to favour keeping the test surface stable; the plugin
        // dispatch path is also the shortest-running of the remaining
        // bridges (handler bodies are user-controlled Lua and almost
        // always complete in < 50 ms outside of intentional
        // `narwhal.sql_run` round-trips). Tracked as a follow-up.
        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async move { plugins.dispatch(&command_owned, ctx).await })
        });
        match outcome {
            Ok(PluginCommandOutcome::Status { message }) => {
                self.status.message = message;
            }
            Ok(PluginCommandOutcome::InsertSql { sql, append }) => {
                if !append {
                    self.tabs[self.active_tab].editor.clear();
                }
                self.tabs[self.active_tab].editor.insert_str(&sql);
                self.status.message = format!("plugin inserted {} char(s) of SQL", sql.len());
            }
            Ok(PluginCommandOutcome::Silent) => {}
            Err(PluginError::Unknown(name)) => {
                self.status.message = format!("unknown command: {name}");
            }
            Err(PluginError::Timeout { elapsed_secs }) => {
                self.status.message = format!(
                    "plugin `{plugin_name}` exceeded execution timeout ({elapsed_secs:.1}s); \
                     adjust with `narwhal.set_timeout(secs)` in the plugin script"
                );
            }
            Err(error) => {
                self.status.message = format!("plugin error: {error}");
            }
            // Future PluginCommandOutcome variants: silent fallback.
            Ok(_) => {}
        }
    }
}
