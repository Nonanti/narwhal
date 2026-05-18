# Sample narwhal plugins

Drop any of these files into `~/.config/narwhal/plugins/` (or on Linux,
`$XDG_CONFIG_HOME/narwhal/plugins/`) and they'll auto-load at start-up.
The directory is created for you the first time narwhal runs.

Every script gets a `narwhal` global with three entry points:

```lua
narwhal.register_command(name, description, handler)
    -- handler(arg : string)
    --   return "..."                 -> status bar message
    --   return { sql = "..." }       -> replace editor buffer
    --   return { sql = "...", append = true }
    --                                -> append to editor buffer
    --   return { status = "..." }    -> same as plain string
    --   return nil | false           -> silent

narwhal.register_transform(handler)
    -- handler(result : table)
    -- result.columns       : { { name, data_type }, ... }
    -- result.rows          : { { cell, cell, ... }, ... }
    -- result.rows_affected : integer | nil
    -- result.elapsed_ms    : integer
    -- mutate in place; the return value is ignored

narwhal.sql_run(sql : string) -> result
    -- Run `sql` against the active connection synchronously and
    -- return the same table shape used by register_transform.
    -- Raises a Lua error if no connection is open or the driver
    -- refuses the statement. Bypasses any open :begin transaction
    -- and gets its own pool connection.
```

## The samples

| File | What it does |
|---|---|
| `uppercase.lua` | Result transform that uppercases every TEXT cell. Useful smoke test that the transform hook works on your install. |
| `format_json.lua` | Transform that pretty-prints any cell that parses as JSON. Pure Lua, no deps. |
| `row_count.lua` | `:rc <table>` command. Uses `narwhal.sql_run` to count rows of the given table and reports the number in the status bar. Shows the executor in action. |
| `query_snippet.lua` | `:top <table>` injects `SELECT * FROM <table> LIMIT 10` into the editor. Handy daily-driver snippet pattern. |

## Loading without auto-load

You can also load a plugin on demand from the `:` prompt:

```
:plug-load /tmp/playground.lua
:plug-list
```

`:plug-list` shows every command every loaded plugin exposes.

## Writing your own

The fastest feedback loop is `:plug-load` while narwhal is open —
each call replaces the prior copy in the registry only if the script
chose a different `name` (the file stem). If you edit a plugin and
reload it without changing the name, the second `:plug-load` fails
with a "command already registered" error. Either restart narwhal or
pick a new file stem for testing.

Scripts run inside `tokio::task::spawn_blocking`, so a misbehaving
plugin can hang its own dispatch but won't deadlock the TUI. SQL
issued via `narwhal.sql_run` goes through the pool's normal queue —
the same one statements typed into the editor use.

### Constraints to keep in mind

- **Reserved names**: you can't register a command that shadows a
  built-in (`run`, `open`, `begin`, `commit`, `quit`, `help`, …).
  The plugin will be rejected at load time with a clear message.
- **No `sql_run` during `:begin`**: while a transaction is open the
  executor refuses `narwhal.sql_run` because a fresh pool connection
  wouldn't see the pinned transaction's writes. Plain commands and
  transforms keep working; only the SQL bridge is gated.
- **Whole-result-in-memory**: `narwhal.sql_run` materialises every
  row before handing it back to Lua. Use `LIMIT` for scans — streaming
  from Lua is a future addition.
- **SQL injection is your problem**: anything you splice into a SQL
  string from `arg` is unescaped user input. `row_count.lua` and
  `query_snippet.lua` both show the whitelist-then-quote pattern;
  copy it for any plugin that takes a table or column name.

### Quick template

```lua
local function safe_ident(s)
    if s == nil then return nil, "name required" end
    if s:match("^[%a_][%w_]*$") == nil then
        return nil, ("invalid name '%s'"):format(s)
    end
    return s
end

narwhal.register_command("mycmd", "what it does", function(arg)
    local name, err = safe_ident(arg:match("^%s*(.-)%s*$"))
    if name == nil then return "mycmd: " .. err end
    -- …do something with `name`…
    return "done"
end)
```
