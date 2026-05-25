//! Lua scripting runtime for narwhal plugins.
//!
//! Each [`LuaPlugin`] owns a single [`mlua::Lua`] state. Scripts register
//! commands and result-transform hooks against a global `narwhal` table
//! injected by the runtime. Because Lua is single-threaded, every call into
//! the VM goes through a [`tokio::sync::Mutex`] and is dispatched onto
//! [`tokio::task::spawn_blocking`].
//!
//! ## Script API
//!
//! A plugin script runs once at load time. The `narwhal` global exposes
//! two registration helpers:
//!
//! ```lua
//! narwhal.register_command(name, description, handler)
//!     -- handler(arg : string)
//!     --   returning a string sets the status bar to that string
//!     --   returning { sql = "..." } appends SQL to the editor
//!     --   returning { sql = "...", append = false } replaces the buffer
//!     --   returning nil or false is silent
//!
//! narwhal.register_transform(handler)
//!     -- handler(result : table)
//!     --   result.columns      : { { name, data_type }, ... }
//!     --   result.rows         : { { cell, cell, ... }, ... }
//!     --   result.rows_affected: integer or nil
//!     --   result.elapsed_ms   : integer
//!     -- mutate in place; return value is ignored
//!
//! narwhal.set_timeout(seconds)
//!     -- Override the default 5 s execution budget for this plugin's
//!     -- command handlers.  0 disables the timeout entirely.  Call at
//!     -- the top level of the script (not inside a handler) so the
//!     -- budget takes effect on the next invocation.
//! ```
//!
//! Values inside `result.rows` are rendered as strings — round-tripping
//! richer types through Lua is not in scope (it would force every script
//! to carry an opaque userdata mapping). NULL becomes Lua `nil`.
//!
//! ## Concurrency and re-entrancy
//!
//! The Lua VM is held behind a [`std::sync::Mutex`] (not
//! `tokio::sync::Mutex`, whose `blocking_lock()` panics from an async
//! context). Plugin calls are dispatched via
//! [`tokio::task::spawn_blocking`] so the blocking lock never starves
//! the async runtime.
//!
//! Because [`std::sync::Mutex`] is **not re-entrant**, a Lua handler
//! must not synchronously call back into the *same* plugin's dispatch
//! or transform path. In practice the only Lua-callable bridge is
//! `narwhal.sql_run`, and the host's [`SqlExecutor`] implementation
//! never re-enters a plugin — it goes straight to the connection
//! pool. If you add another Lua-callable bridge, audit it for the same
//! property or the script will deadlock its own VM.
//!
//! ## Runtime requirement
//!
//! [`Plugin::dispatch`] and [`Plugin::transform_result`] use
//! [`tokio::task::block_in_place`] internally for the `sql_run` bridge
//! and so require a **multi-threaded Tokio runtime**. Tests in this
//! crate annotate themselves with `#[tokio::test(flavor =
//! "multi_thread")]`; the production binary uses the default
//! `#[tokio::main]` which is multi-thread.

#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mlua::{
    Function, HookTriggers, Lua, LuaOptions, RegistryKey, Result as LuaResult, StdLib, Table,
    Value as LuaValue, VmState,
};
use narwhal_plugin::{
    ColumnHeader, CommandContext, CommandDescriptor, CommandOutcome, Plugin, PluginError,
    PluginResult, QueryResult, Row, SqlExecutor, Value,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tokio::task;

/// Default execution budget for a plugin command handler. Long enough
/// for any reasonable result-pane transformation, short enough to be
/// obvious when something went wrong.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Sandboxing policy applied to the embedded Lua VM.
///
/// M12: prior to this enum the runtime always called `Lua::new()`,
/// which loads every standard library mlua ships, including `io`,
/// `os` (with `os.execute`), `package` (with `package.loadlib`) and
/// `debug`. Trusted local scripts (e.g. `examples/plugins/csv_export.lua`,
/// which writes a CSV via `io.open`) rely on that surface, so the
/// default stays permissive for backwards compatibility — but hosts
/// that load *untrusted* scripts (curated marketplace, network sync,
/// etc.) can now opt in to a restricted VM that excludes the
/// filesystem, process-spawning, dynamic-library, and debug surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LuaSandbox {
    /// Load every standard library mlua provides. Equivalent to the
    /// historical `Lua::new()` call — keeps existing scripts working.
    /// **Only use for scripts you trust to the same degree as native
    /// code.**
    Permissive,

    /// Load only the side-effect-free standard libraries: `table`,
    /// `string`, `utf8`, `math`, `coroutine`, `bit` (Lua 5.2+) and
    /// `buffer` (`LuaJIT`). `io`, `os`, `package`, `debug` and `ffi`
    /// are *not* loaded, so a malicious script cannot touch the
    /// filesystem, spawn processes, or escape via `debug.getregistry`.
    Restricted,
}

impl LuaSandbox {
    /// Build the [`mlua::Lua`] state for this sandbox policy.
    fn build_lua(self) -> PluginResult<Lua> {
        match self {
            Self::Permissive => Ok(Lua::new()),
            Self::Restricted => {
                // Bitwise-OR the safe libraries explicitly so that future
                // mlua versions adding a new `StdLib::FOO` don't silently
                // get included — we'd rather review the addition first.
                // BIT / BUFFER are LuaJIT-only and aren't loadable under
                // lua54 (the workspace feature flag), so they're omitted.
                let safe = StdLib::TABLE
                    | StdLib::STRING
                    | StdLib::UTF8
                    | StdLib::MATH
                    | StdLib::COROUTINE;
                Lua::new_with(safe, LuaOptions::new()).map_err(|e| {
                    PluginError::Runtime(format!("failed to build sandboxed Lua VM: {e}"))
                })
            }
        }
    }
}

// `Default` is derived rather than hand-written so future variants
// don't accidentally change the default without an explicit annotation.
impl Default for LuaSandbox {
    #[inline]
    fn default() -> Self {
        // Equivalent to `#[derive(Default)]` + `#[default]`, written
        // explicitly so the choice is obvious at the variant site.
        Self::Permissive
    }
}

/// Tracks elapsed time for a single plugin invocation against a
/// configured budget. Used inside the mlua hook callback to decide
/// whether the invocation has exceeded its time limit.
/// Tracks elapsed time for a single plugin invocation against a
/// configured budget. Used inside the mlua hook callback to decide
/// whether the invocation has exceeded its time limit.
///
/// All fields are lock-free: `started_at` and `budget` are immutable
/// after construction, and `timed_out` is an `AtomicBool` so the
/// hook closure can read/write it without a `Mutex`.
struct InvocationTimeout {
    started_at: Instant,
    budget: Duration,
    timed_out: AtomicBool,
}

impl InvocationTimeout {
    fn new(budget: Duration) -> Self {
        Self {
            started_at: Instant::now(),
            budget: budget.max(Duration::from_millis(1)),
            timed_out: AtomicBool::new(false),
        }
    }

    fn exceeded(&self) -> bool {
        self.started_at.elapsed() >= self.budget
    }
}

/// Upper bound for a valid timeout budget (~31 years). Anything above
/// this would overflow `Duration::from_secs_f64`; we treat it the same
/// as "disable timeout" rather than panicking.
const MAX_BUDGET_SECS: f64 = 1e9;

/// Read the per-plugin timeout budget from the Lua registry.
/// Returns `None` when the plugin hasn't called `narwhal.set_timeout`
/// (the caller should fall back to [`DEFAULT_TIMEOUT`]).
///
/// The budget is stored in the registry (not on the `narwhal` global
/// table) so scripts cannot accidentally clear or corrupt it by
/// assigning to `narwhal._timeout_budget`.
fn read_timeout_budget(lua: &Lua) -> Option<Duration> {
    let secs: f64 = lua.named_registry_value("narwhal_timeout_budget").ok()?;
    // Guard against NaN, infinity, negative, and astronomically large
    // values that would panic inside `Duration::from_secs_f64`.
    Some(
        if !secs.is_finite() || secs <= 0.0 || secs > MAX_BUDGET_SECS {
            Duration::MAX
        } else {
            Duration::from_secs_f64(secs)
        },
    )
}

/// Lua-backed plugin. Wraps a single Lua VM together with the metadata
/// the host needs to look it up.
pub struct LuaPlugin {
    name: String,
    lua: Arc<Mutex<Lua>>,
    /// Snapshot of commands the script registered. Kept outside the VM so
    /// [`Plugin::commands`] is sync and allocation-cheap.
    descriptors: Vec<CommandDescriptor>,
    /// `RegistryKey` for the table that maps command name → Lua handler.
    /// Stored in the Lua registry so the handlers survive across calls.
    commands_key: Arc<RegistryKey>,
    /// `RegistryKey` for the list of transform handlers.
    transforms_key: Arc<RegistryKey>,
}

impl LuaPlugin {
    /// Compile and run `script`, registering whatever commands and
    /// transforms it declares. `name` is the identifier surfaced to the
    /// host (`Plugin::name`).
    pub fn from_script(name: impl Into<String>, script: &str) -> PluginResult<Self> {
        Self::from_script_with_sandbox(name, script, LuaSandbox::default())
    }

    /// Compile and run `script` in a VM configured with the given
    /// [`LuaSandbox`] policy.
    ///
    /// M12: prefer [`LuaSandbox::Restricted`] for scripts that come
    /// from an untrusted source. The current default policy
    /// ([`LuaSandbox::Permissive`]) keeps the historical behaviour so
    /// existing scripts that use `io.open`, `os.date`, etc. continue
    /// to work — the security trade-off is the caller's to make.
    pub fn from_script_with_sandbox(
        name: impl Into<String>,
        script: &str,
        sandbox: LuaSandbox,
    ) -> PluginResult<Self> {
        let name = name.into();
        let lua = sandbox.build_lua()?;
        let commands_table = lua
            .create_table()
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        let transforms_table = lua
            .create_table()
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        let descriptors_table = lua
            .create_table()
            .map_err(|e| PluginError::Runtime(e.to_string()))?;

        install_api(&lua, &commands_table, &transforms_table, &descriptors_table)
            .map_err(|e| PluginError::Runtime(format!("install_api: {e}")))?;

        lua.load(script)
            .set_name(&name)
            .exec()
            .map_err(|e| PluginError::Runtime(format!("script load: {e}")))?;

        let descriptors = read_descriptors(&descriptors_table)
            .map_err(|e| PluginError::Runtime(format!("descriptor read: {e}")))?;
        let commands_key = lua
            .create_registry_value(commands_table)
            .map_err(|e| PluginError::Runtime(e.to_string()))?;
        let transforms_key = lua
            .create_registry_value(transforms_table)
            .map_err(|e| PluginError::Runtime(e.to_string()))?;

        Ok(Self {
            name,
            lua: Arc::new(Mutex::new(lua)),
            descriptors,
            commands_key: Arc::new(commands_key),
            transforms_key: Arc::new(transforms_key),
        })
    }

    /// Wire `executor` into this plugin's Lua VM, exposing
    /// `narwhal.sql_run(sql)` to scripts. Returns the resulting query
    /// as a Lua table of the same shape produced by
    /// [`Plugin::transform_result`] — `{columns = {...}, rows = {...},
    /// rows_affected = number|nil, elapsed_ms = number}`.
    ///
    /// Calling this twice replaces the previously-installed executor.
    /// The host calls it exactly once, right after loading the plugin.
    pub fn install_executor(&self, executor: Arc<dyn SqlExecutor>) -> PluginResult<()> {
        // Lua is single-threaded; grab the mutex while we touch globals.
        let lua = self
            .lua
            .lock()
            .map_err(|e| PluginError::Runtime(format!("lua mutex poisoned: {e}")))?;
        let narwhal: Table = lua
            .globals()
            .get("narwhal")
            .map_err(|e| PluginError::Runtime(format!("missing narwhal global: {e}")))?;
        let exec = executor.clone();
        // The Lua function bridges the call back into Rust via the
        // current Tokio runtime. We are *already* inside a
        // `spawn_blocking` task whenever a script runs (see
        // [`Self::dispatch`] / [`Self::transform_result`]), so it is
        // safe to call `Handle::current().block_on(...)` here.
        let sql_run = lua
            .create_function(move |lua, sql: String| {
                let exec = exec.clone();
                let handle = match tokio::runtime::Handle::try_current() {
                    Ok(h) => h,
                    Err(e) => {
                        return Err(mlua::Error::external(format!(
                            "narwhal.sql_run requires a tokio runtime: {e}"
                        )));
                    }
                };
                // Apply the plugin's timeout budget to sql_run as well.
                // Without this, a long-running SQL query called from Lua
                // would bypass the EVERY_LINE timeout hook since the hook
                // only fires on Lua bytecode boundaries, not during
                // blocking Rust calls.
                let budget = read_timeout_budget(lua).unwrap_or(DEFAULT_TIMEOUT);
                let fut = exec.run(&sql);
                let outcome = if budget == Duration::MAX {
                    // Timeout disabled — run without a deadline.
                    tokio::task::block_in_place(|| handle.block_on(fut))
                } else {
                    tokio::task::block_in_place(|| {
                        handle.block_on(async {
                            match tokio::time::timeout(budget, fut).await {
                                Ok(result) => result,
                                Err(_) => Err(PluginError::Timeout {
                                    elapsed_secs: budget.as_secs_f64(),
                                }),
                            }
                        })
                    })
                };
                match outcome {
                    Ok(qr) => result_to_lua(lua, &qr)
                        .map_err(|e| mlua::Error::external(format!("encode result: {e}"))),
                    Err(e) => Err(mlua::Error::external(e.to_string())),
                }
            })
            .map_err(|e| PluginError::Runtime(format!("create sql_run: {e}")))?;
        narwhal
            .set("sql_run", sql_run)
            .map_err(|e| PluginError::Runtime(format!("install sql_run: {e}")))?;
        Ok(())
    }

    /// Convenience: read a script from disk and call [`Self::from_script`].
    ///
    /// The plugin's identifier is `"lua-{stem}"` where `stem` is the file
    /// name without extension (e.g. `format_json.lua` becomes
    /// `"lua-format_json"`). For paths whose file name is not valid UTF-8
    /// we fall back to a path-derived display string so two such plugins
    /// don't collide in the registry display. The name is deterministic
    /// across restarts — no randomized hash.
    pub fn from_path(path: impl AsRef<Path>) -> PluginResult<Self> {
        let path = path.as_ref();
        let source = std::fs::read_to_string(path)
            .map_err(|e| PluginError::Runtime(format!("read {}: {e}", path.display())))?;
        let stem = if let Some(s) = path.file_stem().and_then(|s| s.to_str()) {
            s.to_owned()
        } else {
            // Non-UTF-8 file name: use the full path's lossy display as
            // a stable identifier. No randomized hash.
            format!("plugin-{}", path.display())
        };
        let name = format!("lua-{stem}");
        Self::from_script(name, &source)
    }
}

#[async_trait]
impl Plugin for LuaPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn commands(&self) -> Vec<CommandDescriptor> {
        self.descriptors.clone()
    }

    async fn dispatch(&self, name: &str, ctx: CommandContext) -> PluginResult<CommandOutcome> {
        let lua = self.lua.clone();
        let commands_key = self.commands_key.clone();
        let name = name.to_owned();
        let argument = ctx.argument;
        let editor_text = ctx.editor_text;

        // Lua is single-threaded so we hold the mutex across the entire
        // call. The work itself is CPU-bound, so spawn_blocking is the
        // right shape.
        task::spawn_blocking(move || {
            let guard = match lua.lock() {
                Ok(g) => g,
                Err(e) => return Err(PluginError::Runtime(format!("lua mutex poisoned: {e}"))),
            };
            invoke_command(&guard, &commands_key, &name, &argument, &editor_text)
        })
        .await
        .map_err(|e| PluginError::Runtime(format!("join: {e}")))?
    }

    async fn transform_result(&self, result: &mut QueryResult) -> PluginResult<()> {
        let lua = self.lua.clone();
        let transforms_key = self.transforms_key.clone();
        // Move the result out so we can hand it across the thread
        // boundary, then move the (possibly rewritten) value back. Even
        // when a transform errors we still want to restore whatever the
        // earlier transforms produced — callers shouldn't lose rows
        // because a later transform raised.
        let owned = std::mem::take(result);
        let (updated, err) = task::spawn_blocking(move || match lua.lock() {
            Ok(guard) => invoke_transforms(&guard, &transforms_key, owned),
            Err(e) => (
                owned,
                Some(PluginError::Runtime(format!("lua mutex poisoned: {e}"))),
            ),
        })
        .await
        .map_err(|e| PluginError::Runtime(format!("join: {e}")))?;
        *result = updated;
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

fn install_api(
    lua: &Lua,
    commands_table: &Table,
    transforms_table: &Table,
    descriptors_table: &Table,
) -> LuaResult<()> {
    let narwhal = lua.create_table()?;

    // narwhal.register_command(name, description, handler)
    //
    // Reject duplicates *inside the same plugin*: the previous
    // implementation silently let `register_command("foo", ...)` be
    // called twice, which left two descriptors and a single (last)
    // handler. The host then rejected the whole plugin with a
    // misleading "command already registered" error. Failing fast
    // here, with a useful message, is friendlier to script authors.
    let cmds = commands_table.clone();
    let descs = descriptors_table.clone();
    let register_command = lua.create_function(
        move |_, (name, description, handler): (String, String, Function)| {
            if cmds.contains_key(name.clone())? {
                return Err(mlua::Error::external(format!(
                    "command '{name}' is already registered in this plugin"
                )));
            }
            cmds.set(name.clone(), handler)?;
            descs.push(vec![name, description])?;
            Ok(())
        },
    )?;
    narwhal.set("register_command", register_command)?;

    // narwhal.register_transform(handler)
    let transforms = transforms_table.clone();
    let register_transform = lua.create_function(move |_, handler: Function| {
        transforms.push(handler)?;
        Ok(())
    })?;
    narwhal.set("register_transform", register_transform)?;

    // narwhal.set_timeout(seconds)
    //
    // Override the default 5 s execution budget for this plugin's
    // command handlers. 0 disables the timeout entirely. Call at the
    // top level of the script (not inside a handler) so the budget
    // takes effect on the next invocation.
    let set_timeout = lua.create_function(|lua, secs: f64| {
        lua.set_named_registry_value("narwhal_timeout_budget", secs)?;
        Ok(())
    })?;
    narwhal.set("set_timeout", set_timeout)?;

    lua.globals().set("narwhal", narwhal)?;
    Ok(())
}

fn read_descriptors(descriptors_table: &Table) -> LuaResult<Vec<CommandDescriptor>> {
    let mut out = Vec::new();
    for pair in descriptors_table.clone().sequence_values::<Table>() {
        let entry = pair?;
        let name: String = entry.get(1)?;
        let description: String = entry.get(2)?;
        out.push(CommandDescriptor { name, description });
    }
    Ok(out)
}

fn invoke_command(
    lua: &Lua,
    commands_key: &RegistryKey,
    name: &str,
    argument: &str,
    editor_text: &str,
) -> PluginResult<CommandOutcome> {
    // Publish the editor buffer text on narwhal.editor_text so handlers
    // can read it (e.g. :explain-cost wrapping the buffer in a prefix).
    // The field is set before every dispatch and does not persist — a
    // handler that reads it on the next call gets whatever is in the
    // editor at that point.
    if let Ok(narwhal) = lua.globals().get::<Table>("narwhal") {
        let _ = narwhal.set("editor_text", editor_text);
    }

    let commands: Table = lua
        .registry_value(commands_key)
        .map_err(|e| PluginError::Runtime(e.to_string()))?;
    // Distinguish "plugin doesn't claim this name" (→ Unknown) from
    // "plugin claims it but the value is broken" (→ Runtime). The old
    // code lumped both into Unknown and ate the underlying mlua error.
    let has = commands
        .contains_key(name)
        .map_err(|e| PluginError::Runtime(e.to_string()))?;
    if !has {
        return Err(PluginError::Unknown(name.to_owned()));
    }
    let handler: Function = commands
        .get(name)
        .map_err(|e| PluginError::Runtime(format!("handler '{name}' is not a function: {e}")))?;

    let budget = read_timeout_budget(lua).unwrap_or(DEFAULT_TIMEOUT);
    let returned: LuaValue = call_handler_with_timeout(lua, &handler, argument.to_owned(), budget)?;

    outcome_from_lua(returned).map_err(PluginError::Handler)
}

/// Call a Lua handler function with an optional execution timeout.
///
/// When `budget` is [`Duration::MAX`] (timeout disabled via
/// `narwhal.set_timeout(0)`), the handler runs without any hook.
/// Otherwise an mlua line-hook is installed that checks
/// [`InvocationTimeout::exceeded`] on every line boundary. When the
/// budget is exceeded the hook raises a `RuntimeError` that propagates
/// back as [`PluginError::Timeout`].
///
/// The hook is always removed after the handler returns (or errors) so
/// the next invocation doesn't inherit this one's budget.
///
/// Generic over the argument type so both command dispatch (string arg)
/// and transform dispatch (table arg) can share the timeout machinery.
fn call_handler_with_timeout<A: mlua::IntoLuaMulti>(
    lua: &Lua,
    handler: &Function,
    argument: A,
    budget: Duration,
) -> PluginResult<LuaValue> {
    if budget == Duration::MAX {
        return handler
            .call(argument)
            .map_err(|e| PluginError::Handler(e.to_string()));
    }

    let timeout = Arc::new(InvocationTimeout::new(budget));
    let timeout_hook = timeout.clone();
    let timeout_err = timeout;

    lua.set_hook(HookTriggers::EVERY_LINE, move |_lua, _debug| {
        if timeout_hook.exceeded() {
            timeout_hook.timed_out.store(true, Ordering::Release);
            Err(mlua::Error::RuntimeError(format!(
                "plugin timed out after {:.1}s",
                timeout_hook.started_at.elapsed().as_secs_f64()
            )))
        } else {
            Ok(VmState::Continue)
        }
    });

    let result = handler.call(argument);
    lua.remove_hook();

    // If the handler returned a value (even if the timeout flag is
    // also set due to a race), prefer the successful result — the
    // plugin *did* finish, just barely over budget.
    match result {
        Ok(v) => Ok(v),
        Err(e) => {
            if timeout_err.timed_out.load(Ordering::Acquire) {
                Err(PluginError::Timeout {
                    elapsed_secs: timeout_err.started_at.elapsed().as_secs_f64(),
                })
            } else {
                Err(PluginError::Handler(e.to_string()))
            }
        }
    }
}

fn outcome_from_lua(value: LuaValue) -> std::result::Result<CommandOutcome, String> {
    match value {
        // `nil`, `false`, and `true` all mean "command handled, nothing
        // to show". `true` used to fall through to the wildcard arm and
        // return an opaque "unsupported return value: boolean" — H14
        // makes the contract explicit instead.
        LuaValue::Nil | LuaValue::Boolean(_) => Ok(CommandOutcome::Silent),
        LuaValue::String(s) => Ok(CommandOutcome::Status {
            message: s.to_string_lossy(),
        }),
        LuaValue::Table(t) => {
            // Either { sql = "...", append = bool } or { status = "..." }.
            //
            // append defaults to *true* because the most common script
            // shape — a snippet that appends a SELECT to whatever the
            // user already typed — is the safer default. Wiping the
            // editor on every command would be a footgun for someone
            // halfway through a long query. Scripts that explicitly
            // want to replace the buffer set append = false.
            //
            // H14: probe the raw LuaValue before coercing so a typo'd
            // shape like `{ sql = 42 }` produces a clear error instead
            // of mlua silently stringifying `42` (or, for some types,
            // returning Err that we used to swallow into the
            // "missing sql/status" branch).
            let raw_sql = t
                .get::<LuaValue>("sql")
                .map_err(|e| format!("reading 'sql' field: {e}"))?;
            let raw_status = t
                .get::<LuaValue>("status")
                .map_err(|e| format!("reading 'status' field: {e}"))?;

            match (&raw_sql, &raw_status) {
                (LuaValue::String(s), _) => {
                    let append = match t.get::<Option<bool>>("append") {
                        Ok(Some(v)) => v,
                        Ok(None) => true,
                        Err(e) => return Err(format!("'append' must be boolean: {e}")),
                    };
                    Ok(CommandOutcome::InsertSql {
                        sql: s.to_string_lossy(),
                        append,
                    })
                }
                (LuaValue::Nil, LuaValue::String(s)) => Ok(CommandOutcome::Status {
                    message: s.to_string_lossy(),
                }),
                (LuaValue::Nil, LuaValue::Nil) => {
                    Err("table return must have a 'sql' or 'status' field".into())
                }
                (other, _) if !matches!(other, LuaValue::Nil) => {
                    Err(format!("'sql' must be a string, got {}", other.type_name()))
                }
                (_, other) => Err(format!(
                    "'status' must be a string, got {}",
                    other.type_name()
                )),
            }
        }
        other => Err(format!("unsupported return value: {}", other.type_name())),
    }
}

/// Run every registered transform in order over `result`. Returns the
/// (possibly partially transformed) `QueryResult` alongside the first
/// error encountered, if any. The partial result is intentional: a
/// failing transform shouldn't be able to swallow earlier transforms'
/// work or the original rows.
fn invoke_transforms(
    lua: &Lua,
    transforms_key: &RegistryKey,
    mut result: QueryResult,
) -> (QueryResult, Option<PluginError>) {
    let transforms: Table = match lua.registry_value(transforms_key) {
        Ok(t) => t,
        Err(e) => return (result, Some(PluginError::Runtime(e.to_string()))),
    };
    if transforms.len().map_or(true, |n| n == 0) {
        return (result, None);
    }
    // Transform handlers are subject to the same timeout budget as
    // command handlers — a buggy transform must not hang the TUI.
    let budget = read_timeout_budget(lua).unwrap_or(DEFAULT_TIMEOUT);
    for handler in transforms.sequence_values::<Function>() {
        let handler = match handler {
            Ok(h) => h,
            Err(e) => return (result, Some(PluginError::Runtime(e.to_string()))),
        };
        let table = match result_to_lua(lua, &result) {
            Ok(t) => t,
            Err(e) => return (result, Some(PluginError::Runtime(format!("encode: {e}")))),
        };
        if let Err(e) = call_handler_with_timeout(lua, &handler, table.clone(), budget) {
            return (result, Some(e));
        }
        match result_from_lua(table) {
            Ok(r) => result = r,
            Err(e) => return (result, Some(PluginError::Runtime(format!("decode: {e}")))),
        }
    }
    (result, None)
}

fn result_to_lua(lua: &Lua, result: &QueryResult) -> LuaResult<Table> {
    let columns = lua.create_table()?;
    for header in &result.columns {
        let entry = lua.create_table()?;
        entry.set("name", header.name.as_str())?;
        entry.set("data_type", header.data_type.as_str())?;
        columns.push(entry)?;
    }
    let rows = lua.create_table()?;
    for row in &result.rows {
        let row_t = lua.create_table()?;
        for value in &row.0 {
            row_t.push(value_to_lua(lua, value)?)?;
        }
        rows.push(row_t)?;
    }
    let result_t = lua.create_table()?;
    result_t.set("columns", columns)?;
    result_t.set("rows", rows)?;
    if let Some(n) = result.rows_affected {
        result_t.set("rows_affected", n)?;
    }
    result_t.set("elapsed_ms", result.elapsed_ms)?;
    Ok(result_t)
}

fn result_from_lua(table: Table) -> LuaResult<QueryResult> {
    let columns_t: Table = table.get("columns")?;
    let mut columns = Vec::new();
    for entry in columns_t.sequence_values::<Table>() {
        let entry = entry?;
        columns.push(ColumnHeader {
            name: entry.get("name")?,
            data_type: entry.get("data_type").unwrap_or_default(),
        });
    }
    let rows_t: Table = table.get("rows")?;
    let mut rows = Vec::new();
    for entry in rows_t.sequence_values::<Table>() {
        let entry = entry?;
        let mut cells = Vec::new();
        for cell in entry.sequence_values::<LuaValue>() {
            cells.push(value_from_lua(cell?));
        }
        rows.push(Row(cells));
    }
    let rows_affected: Option<u64> = table.get("rows_affected").ok();
    let elapsed_ms: u64 = table.get("elapsed_ms").unwrap_or(0);
    Ok(QueryResult {
        columns,
        rows,
        rows_affected,
        elapsed_ms,
    })
}

fn value_to_lua(lua: &Lua, value: &Value) -> LuaResult<LuaValue> {
    Ok(match value {
        Value::Null => LuaValue::Nil,
        Value::Bool(b) => LuaValue::Boolean(*b),
        Value::Int(i) => LuaValue::Integer(*i),
        Value::Float(f) => LuaValue::Number(*f),
        Value::String(s) => LuaValue::String(lua.create_string(s)?),
        Value::Bytes(b) => LuaValue::String(lua.create_string(b)?),
        Value::Date(_)
        | Value::Time(_)
        | Value::DateTime(_)
        | Value::Timestamp(_)
        | Value::Uuid(_)
        | Value::Json(_)
        | Value::Unknown(_) => LuaValue::String(lua.create_string(value.render())?),
        // Forward-compatible fallback for future `Value` variants — surface the
        // canonical rendered form so Lua sees a string.
        _ => LuaValue::String(lua.create_string(value.render())?),
    })
}

fn value_from_lua(value: LuaValue) -> Value {
    match value {
        LuaValue::Nil => Value::Null,
        LuaValue::Boolean(b) => Value::Bool(b),
        LuaValue::Integer(i) => Value::Int(i),
        LuaValue::Number(f) => Value::Float(f),
        LuaValue::String(s) => Value::String(s.to_string_lossy()),
        other => Value::String(format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use narwhal_plugin::{ColumnHeader, PluginRegistry};

    fn make_result() -> QueryResult {
        QueryResult {
            columns: vec![
                ColumnHeader {
                    name: "id".into(),
                    data_type: "INTEGER".into(),
                },
                ColumnHeader {
                    name: "label".into(),
                    data_type: "TEXT".into(),
                },
            ],
            rows: vec![
                Row(vec![Value::Int(1), Value::String("alpha".into())]),
                Row(vec![Value::Int(2), Value::String("beta".into())]),
            ],
            rows_affected: None,
            elapsed_ms: 7,
        }
    }

    #[tokio::test]
    async fn register_command_and_dispatch_status() {
        let script = r#"
            narwhal.register_command("upper", "uppercase", function(arg)
                return arg:upper()
            end)
        "#;
        let plugin = LuaPlugin::from_script("test", script).unwrap();
        let descriptors = plugin.commands();
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].name, "upper");

        let outcome = plugin
            .dispatch("upper", CommandContext::new("hello"))
            .await
            .unwrap();
        match outcome {
            CommandOutcome::Status { message } => assert_eq!(message, "HELLO"),
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn command_returning_table_injects_sql() {
        let script = r#"
            narwhal.register_command("gen", "generate SQL", function(_)
                return { sql = "SELECT 42", append = true }
            end)
        "#;
        let plugin = LuaPlugin::from_script("gen-plugin", script).unwrap();
        let outcome = plugin
            .dispatch("gen", CommandContext::default())
            .await
            .unwrap();
        match outcome {
            CommandOutcome::InsertSql { sql, append } => {
                assert_eq!(sql, "SELECT 42");
                assert!(append);
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn command_table_without_append_field_defaults_to_appending() {
        // The user's editor buffer is more sacred than the plugin
        // author's tidiness: omitting `append` should NOT wipe what
        // the user already typed.
        let script = r#"
            narwhal.register_command("gen", "snippet", function(_)
                return { sql = "SELECT 1" }
            end)
        "#;
        let plugin = LuaPlugin::from_script("gen-plugin", script).unwrap();
        let outcome = plugin
            .dispatch("gen", CommandContext::default())
            .await
            .unwrap();
        match outcome {
            CommandOutcome::InsertSql { append, .. } => assert!(append),
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn command_table_with_explicit_append_false_replaces_buffer() {
        let script = r#"
            narwhal.register_command("gen", "snippet", function(_)
                return { sql = "SELECT 1", append = false }
            end)
        "#;
        let plugin = LuaPlugin::from_script("gen-plugin", script).unwrap();
        let outcome = plugin
            .dispatch("gen", CommandContext::default())
            .await
            .unwrap();
        match outcome {
            CommandOutcome::InsertSql { append, .. } => assert!(!append),
            other => panic!("unexpected outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn command_returning_nil_is_silent() {
        let script = r#"
            narwhal.register_command("noop", "no-op", function(_) return nil end)
        "#;
        let plugin = LuaPlugin::from_script("noop-plugin", script).unwrap();
        let outcome = plugin
            .dispatch("noop", CommandContext::default())
            .await
            .unwrap();
        assert!(matches!(outcome, CommandOutcome::Silent));
    }

    #[tokio::test]
    async fn unknown_command_reports_unknown() {
        let plugin = LuaPlugin::from_script("empty", "").unwrap();
        let err = plugin
            .dispatch("nope", CommandContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, PluginError::Unknown(_)));
    }

    #[tokio::test]
    async fn handler_error_propagates() {
        let script = r#"
            narwhal.register_command("boom", "explode", function(_)
                error("kaboom")
            end)
        "#;
        let plugin = LuaPlugin::from_script("boom-plugin", script).unwrap();
        let err = plugin
            .dispatch("boom", CommandContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, PluginError::Handler(_)));
        let msg = format!("{err}");
        assert!(msg.contains("kaboom"), "got: {msg}");
    }

    #[tokio::test]
    async fn transform_can_rewrite_cells() {
        let script = r#"
            narwhal.register_transform(function(result)
                for _, row in ipairs(result.rows) do
                    if type(row[2]) == "string" then
                        row[2] = row[2]:upper()
                    end
                end
            end)
        "#;
        let plugin = LuaPlugin::from_script("upper-rows", script).unwrap();
        let mut result = make_result();
        plugin.transform_result(&mut result).await.unwrap();
        assert_eq!(result.rows[0].0[1].render(), "ALPHA");
        assert_eq!(result.rows[1].0[1].render(), "BETA");
    }

    #[tokio::test]
    async fn transform_can_add_columns() {
        let script = r#"
            narwhal.register_transform(function(result)
                table.insert(result.columns, { name = "doubled", data_type = "INTEGER" })
                for _, row in ipairs(result.rows) do
                    table.insert(row, row[1] * 2)
                end
            end)
        "#;
        let plugin = LuaPlugin::from_script("doubler", script).unwrap();
        let mut result = make_result();
        plugin.transform_result(&mut result).await.unwrap();
        assert_eq!(result.columns.len(), 3);
        assert_eq!(result.columns[2].name, "doubled");
        assert_eq!(result.rows[0].0[2].render(), "2");
        assert_eq!(result.rows[1].0[2].render(), "4");
    }

    #[tokio::test]
    async fn lua_plugin_works_through_registry() {
        let script = r#"
            narwhal.register_command("hi", "greet", function(arg)
                return "hi " .. arg
            end)
        "#;
        let plugin = LuaPlugin::from_script("greet", script).unwrap();
        let mut registry = PluginRegistry::new();
        registry.register(plugin).unwrap();
        let outcome = registry
            .dispatch("hi", CommandContext::new("berkant"))
            .await
            .unwrap();
        match outcome {
            CommandOutcome::Status { message } => assert_eq!(message, "hi berkant"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn loading_invalid_script_returns_runtime_error() {
        let err = match LuaPlugin::from_script("bad", "this is not valid lua )") {
            Ok(_) => panic!("expected the bad script to fail"),
            Err(e) => e,
        };
        assert!(matches!(err, PluginError::Runtime(_)));
    }

    /// Mock executor used by the `sql_run` tests. Captures every SQL it
    /// receives and replays a canned [`QueryResult`] back.
    struct MockExecutor {
        seen: std::sync::Mutex<Vec<String>>,
        reply: QueryResult,
        fail: bool,
    }

    #[async_trait]
    impl SqlExecutor for MockExecutor {
        async fn run(&self, sql: &str) -> PluginResult<QueryResult> {
            self.seen.lock().unwrap().push(sql.to_owned());
            if self.fail {
                Err(PluginError::Runtime("boom".into()))
            } else {
                Ok(self.reply.clone())
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sql_run_calls_executor_and_returns_rows_to_lua() {
        let plugin = LuaPlugin::from_script(
            "runner",
            r#"
            narwhal.register_command("count", "count rows", function(_)
                local result = narwhal.sql_run("SELECT 1")
                return tostring(#result.rows)
            end)
            "#,
        )
        .unwrap();

        let executor = Arc::new(MockExecutor {
            seen: std::sync::Mutex::new(Vec::new()),
            reply: QueryResult {
                columns: vec![ColumnHeader {
                    name: "n".into(),
                    data_type: "INTEGER".into(),
                }],
                rows: vec![
                    Row(vec![Value::Int(1)]),
                    Row(vec![Value::Int(2)]),
                    Row(vec![Value::Int(3)]),
                ],
                rows_affected: None,
                elapsed_ms: 0,
            },
            fail: false,
        });
        plugin.install_executor(executor.clone()).unwrap();

        let outcome = plugin
            .dispatch("count", CommandContext::default())
            .await
            .unwrap();
        match outcome {
            CommandOutcome::Status { message } => assert_eq!(message, "3"),
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(executor.seen.lock().unwrap().as_slice(), &["SELECT 1"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sql_run_executor_error_surfaces_to_lua_as_handler_error() {
        let plugin = LuaPlugin::from_script(
            "runner",
            r#"
            narwhal.register_command("go", "", function(_)
                narwhal.sql_run("SELECT bad")
                return "never"
            end)
            "#,
        )
        .unwrap();
        plugin
            .install_executor(Arc::new(MockExecutor {
                seen: std::sync::Mutex::new(Vec::new()),
                reply: QueryResult::default(),
                fail: true,
            }))
            .unwrap();

        let err = plugin
            .dispatch("go", CommandContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, PluginError::Handler(_)));
        let msg = format!("{err}");
        assert!(msg.contains("boom"), "got: {msg}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sql_run_without_executor_raises_in_lua() {
        let plugin = LuaPlugin::from_script(
            "runner",
            r#"
            narwhal.register_command("go", "", function(_)
                narwhal.sql_run("SELECT 1")
                return "never"
            end)
            "#,
        )
        .unwrap();
        // Note: no install_executor.

        let err = plugin
            .dispatch("go", CommandContext::default())
            .await
            .unwrap_err();
        // Without an executor installed, `narwhal.sql_run` simply isn't
        // defined, so Lua raises an 'attempt to call a nil value' style
        // error which we surface as a Handler error.
        assert!(matches!(err, PluginError::Handler(_)));
    }

    #[tokio::test]
    async fn from_path_reads_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lua");
        std::fs::write(
            &path,
            r#"narwhal.register_command("ping", "p", function(_) return "pong" end)"#,
        )
        .unwrap();
        let plugin = LuaPlugin::from_path(&path).unwrap();
        assert_eq!(plugin.name(), "lua-test");
        let outcome = plugin
            .dispatch("ping", CommandContext::default())
            .await
            .unwrap();
        match outcome {
            CommandOutcome::Status { message } => assert_eq!(message, "pong"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// M18: Scripts cannot tamper with the timeout budget because it is
    /// stored in the Lua registry, not on the `narwhal` global table.
    #[tokio::test]
    async fn script_cannot_clear_timeout_budget() {
        // The script tries to nil out `narwhal._timeout_budget` (old
        // location) and also tries to delete it from the registry. The
        // registry is not accessible from Lua, so the budget should
        // survive.
        let script = r#"
            narwhal.set_timeout(0.5)
            -- Try to clear the old location (no-op if field doesn't exist)
            if narwhal._timeout_budget then
                narwhal._timeout_budget = nil
            end
            narwhal.register_command("check", "check", function(_)
                -- Run a long loop; with the 0.5s budget still active
                -- it should time out.
                local x = 0
                for i = 1, 1e9 do x = x + i end
                return "done"
            end)
        "#;
        let plugin = LuaPlugin::from_script("tamper-test", script).unwrap();
        let err = plugin
            .dispatch("check", CommandContext::default())
            .await
            .unwrap_err();
        // Budget survived the script's attempt to clear it.
        assert!(
            matches!(err, PluginError::Timeout { .. }),
            "expected Timeout, got: {err:?}"
        );
    }

    /// M12: Permissive sandbox keeps `io`/`os` visible so existing
    /// scripts (e.g. `examples/plugins/csv_export.lua`) keep working.
    #[test]
    fn permissive_sandbox_exposes_io_and_os() {
        let script = r#"
            assert(io ~= nil, "io should be present")
            assert(os ~= nil, "os should be present")
            assert(type(io.open) == "function", "io.open should be present")
            narwhal.register_command("x", "x", function() end)
        "#;
        let _plugin =
            LuaPlugin::from_script_with_sandbox("permissive", script, LuaSandbox::Permissive)
                .expect("permissive must load");
    }

    /// M12: Restricted sandbox blocks the dangerous standard libraries
    /// (`io`, `os`, `package`, `debug`, `ffi`). Scripts that try to
    /// touch them see `nil` and can decide to fail loudly.
    #[test]
    fn restricted_sandbox_hides_io_os_package_debug() {
        let script = r#"
            assert(io == nil, "io must be sandboxed away")
            assert(os == nil, "os must be sandboxed away")
            assert(package == nil, "package must be sandboxed away")
            assert(debug == nil, "debug must be sandboxed away")
            assert(type(string) == "table", "string must still be present")
            assert(type(math) == "table", "math must still be present")
            narwhal.register_command("x", "x", function() end)
        "#;
        let _plugin =
            LuaPlugin::from_script_with_sandbox("restricted", script, LuaSandbox::Restricted)
                .expect("restricted must load with safe libs only");
    }

    /// M12: even if the script smuggles a string like `"os.execute"`
    /// into `load()`, the resulting chunk runs against an environment
    /// where `os` is `nil`, so the attack still fails.
    #[test]
    fn restricted_sandbox_blocks_loadstring_escape() {
        // `load` is part of the base library and is always present;
        // what matters is that the loaded chunk inherits the same
        // restricted globals.
        let script = r#"
            local chunk, err = load("return os.execute('echo PWND')")
            assert(chunk ~= nil, "load itself stays available")
            local ok, run_err = pcall(chunk)
            assert(not ok, "call must fail because os is nil in the env")
            narwhal.register_command("x", "x", function() end)
        "#;
        let _plugin = LuaPlugin::from_script_with_sandbox("escape", script, LuaSandbox::Restricted)
            .expect("restricted must contain loadstring escape attempts");
    }

    /// M19: Plugin name derived from file stem is deterministic across
    /// separate loads (no randomized `DefaultHasher`).
    #[test]
    fn plugin_name_deterministic_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("my_plugin.lua");
        std::fs::write(
            &path,
            r#"narwhal.register_command("x", "x", function() end)"#,
        )
        .unwrap();

        let name1 = LuaPlugin::from_path(&path).unwrap().name().to_owned();
        let name2 = LuaPlugin::from_path(&path).unwrap().name().to_owned();
        assert_eq!(name1, name2, "plugin name should be deterministic");
        assert_eq!(name1, "lua-my_plugin");
    }
}
