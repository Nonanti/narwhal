-- :rc <table>  -  row count for the given table via narwhal.sql_run
--
-- Demonstrates calling SQL from inside a Lua handler. The connection
-- has to be open; without one the script errors out cleanly.
--
-- Example:
--   :open mydb
--   :rc users
--   -> status bar: users: 4128 row(s)
--
-- ⚠️ SQL injection note: the user's argument lands inside a SQL
-- string. Even with quote_ident wrapping double quotes, an
-- attacker-controlled name could break out. Whitelist-validate
-- before splicing — see `safe_ident` below. Same pattern applies
-- to every plugin that takes a name from `:`-line input.

-- Letters, digits and underscore only. Refuse anything else with a
-- clear error message rather than passing it through to the SQL
-- engine.
local function safe_ident(s)
    if s == nil then return nil, "table name required" end
    if s:match("^[%a_][%w_]*$") == nil then
        return nil, ("invalid table name '%s' (letters, digits, _ only)"):format(s)
    end
    return s
end

local function quote_ident(name)
    -- Defensive even after the whitelist: a future relaxation of
    -- safe_ident still goes through proper quoting.
    return '"' .. name:gsub('"', '""') .. '"'
end

narwhal.register_command("rc", "row count for <table>", function(arg)
    local raw = arg:match("^%s*(.-)%s*$")
    local name, err = safe_ident(raw)
    if name == nil then
        return "rc: " .. err
    end
    local ok, result = pcall(narwhal.sql_run, "SELECT COUNT(*) FROM " .. quote_ident(name))
    if not ok then
        return "rc failed: " .. tostring(result)
    end
    local cell = result.rows[1] and result.rows[1][1]
    return name .. ": " .. tostring(cell) .. " row(s)"
end)
