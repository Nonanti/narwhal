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
//!     --   returning a table { sql = "...", append = true } injects SQL
//!     --   returning nil or false is silent
//!
//! narwhal.register_transform(handler)
//!     -- handler(result : table)
//!     --   result.columns      : { { name, data_type }, ... }
//!     --   result.rows         : { { cell, cell, ... }, ... }
//!     --   result.rows_affected: integer or nil
//!     --   result.elapsed_ms   : integer
//!     -- mutate in place; return value is ignored
//! ```
//!
//! Values inside `result.rows` are rendered as strings — round-tripping
//! richer types through Lua is not in scope (it would force every script
//! to carry an opaque userdata mapping). NULL becomes Lua `nil`.

#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use mlua::{Function, Lua, RegistryKey, Result as LuaResult, Table, Value as LuaValue, Variadic};
use narwhal_plugin::{
    ColumnHeader, CommandContext, CommandDescriptor, CommandOutcome, Plugin, PluginError,
    PluginResult, QueryResult, Row, Value,
};
use tokio::sync::Mutex;
use tokio::task;

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
        let name = name.into();
        let lua = Lua::new();
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

    /// Convenience: read a script from disk and call [`Self::from_script`].
    pub fn from_path(path: impl AsRef<Path>) -> PluginResult<Self> {
        let path = path.as_ref();
        let source = std::fs::read_to_string(path)
            .map_err(|e| PluginError::Runtime(format!("read {}: {e}", path.display())))?;
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("lua-plugin")
            .to_owned();
        Self::from_script(stem, &source)
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

        // Lua is single-threaded so we hold the mutex across the entire
        // call. The work itself is CPU-bound, so spawn_blocking is the
        // right shape.
        task::spawn_blocking(move || {
            let guard = lua.blocking_lock();
            invoke_command(&guard, &commands_key, &name, &argument)
        })
        .await
        .map_err(|e| PluginError::Runtime(format!("join: {e}")))?
    }

    async fn transform_result(&self, result: &mut QueryResult) -> PluginResult<()> {
        if self.descriptors.is_empty() {
            // Fast path: a plugin that only registered commands doesn't
            // need to round-trip through the VM. (Plugins that registered
            // transforms always also have at least one transform; the
            // empty-descriptor case is just a hint, not a guarantee. We
            // still call into Lua below to honour any transforms.)
        }
        let lua = self.lua.clone();
        let transforms_key = self.transforms_key.clone();
        // Move the result out so we can hand it across the thread
        // boundary, then move the (possibly rewritten) value back.
        let owned = std::mem::take(result);
        let updated = task::spawn_blocking(move || {
            let guard = lua.blocking_lock();
            invoke_transforms(&guard, &transforms_key, owned)
        })
        .await
        .map_err(|e| PluginError::Runtime(format!("join: {e}")))??;
        *result = updated;
        Ok(())
    }
}

// ---- Lua API installation ----

fn install_api(
    lua: &Lua,
    commands_table: &Table,
    transforms_table: &Table,
    descriptors_table: &Table,
) -> LuaResult<()> {
    let narwhal = lua.create_table()?;

    // narwhal.register_command(name, description, handler)
    let cmds = commands_table.clone();
    let descs = descriptors_table.clone();
    let register_command = lua.create_function(
        move |_, (name, description, handler): (String, String, Function)| {
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

// ---- command dispatch ----

fn invoke_command(
    lua: &Lua,
    commands_key: &RegistryKey,
    name: &str,
    argument: &str,
) -> PluginResult<CommandOutcome> {
    let commands: Table = lua
        .registry_value(commands_key)
        .map_err(|e| PluginError::Runtime(e.to_string()))?;
    let handler: Function = commands
        .get(name)
        .map_err(|_| PluginError::Unknown(name.to_owned()))?;
    let returned: LuaValue = handler
        .call(argument.to_owned())
        .map_err(|e| PluginError::Handler(e.to_string()))?;
    outcome_from_lua(returned).map_err(PluginError::Handler)
}

fn outcome_from_lua(value: LuaValue) -> std::result::Result<CommandOutcome, String> {
    match value {
        LuaValue::Nil | LuaValue::Boolean(false) => Ok(CommandOutcome::Silent),
        LuaValue::String(s) => Ok(CommandOutcome::Status {
            message: s.to_string_lossy(),
        }),
        LuaValue::Table(t) => {
            // Either { sql = "...", append = bool } or { status = "..." }.
            if let Ok(sql) = t.get::<String>("sql") {
                let append = t.get::<bool>("append").unwrap_or(false);
                Ok(CommandOutcome::InsertSql { sql, append })
            } else if let Ok(status) = t.get::<String>("status") {
                Ok(CommandOutcome::Status { message: status })
            } else {
                Err("table return must have a 'sql' or 'status' field".into())
            }
        }
        other => Err(format!("unsupported return value: {}", other.type_name())),
    }
}

// ---- result transforms ----

fn invoke_transforms(
    lua: &Lua,
    transforms_key: &RegistryKey,
    mut result: QueryResult,
) -> PluginResult<QueryResult> {
    let transforms: Table = lua
        .registry_value(transforms_key)
        .map_err(|e| PluginError::Runtime(e.to_string()))?;
    if transforms.len().map(|n| n == 0).unwrap_or(true) {
        return Ok(result);
    }
    for handler in transforms.sequence_values::<Function>() {
        let handler = handler.map_err(|e| PluginError::Runtime(e.to_string()))?;
        let table = result_to_lua(lua, &result)
            .map_err(|e| PluginError::Runtime(format!("encode: {e}")))?;
        // The script mutates the same table reference; we read the result
        // back from it after the call returns.
        let _: LuaValue = handler
            .call(table.clone())
            .map_err(|e| PluginError::Handler(e.to_string()))?;
        result =
            result_from_lua(table).map_err(|e| PluginError::Runtime(format!("decode: {e}")))?;
    }
    Ok(result)
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
    })
}

fn value_from_lua(value: LuaValue) -> Value {
    match value {
        LuaValue::Nil => Value::Null,
        LuaValue::Boolean(b) => Value::Bool(b),
        LuaValue::Integer(i) => Value::Int(i),
        LuaValue::Number(f) => Value::Float(f),
        LuaValue::String(s) => Value::String(s.to_string_lossy()),
        other => Value::String(format!("{:?}", other)),
    }
}

// Silence the unused-import warning when the lua54 feature is enabled but
// Variadic isn't yet used by the surface API. Future helpers (e.g.
// register_render that takes variadic args) will use it.
#[allow(dead_code)]
fn _variadic_anchor(_: Variadic<LuaValue>) {}

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
        assert_eq!(plugin.name(), "test");
        let outcome = plugin
            .dispatch("ping", CommandContext::default())
            .await
            .unwrap();
        match outcome {
            CommandOutcome::Status { message } => assert_eq!(message, "pong"),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
