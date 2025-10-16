-- :rc <table>  -  row count for the given table via narwhal.sql_run
--
-- Demonstrates calling SQL from inside a Lua handler. The connection
-- has to be open; without one the script errors out cleanly.
--
-- Example:
--   :open mydb
--   :rc users
--   -> status bar: users: 4128 row(s)

local function quote_ident(name)
    -- Very conservative quoting: assumes the table name has no
    -- double-quotes in it. Good enough for the daily case; if you
    -- need full DDL hygiene reach for the dump-schema dialect helpers
    -- instead of a plugin.
    return '"' .. name .. '"'
end

narwhal.register_command("rc", "row count for <table>", function(arg)
    local name = arg:match("^%s*(.-)%s*$")
    if name == "" then
        return "rc: table name required"
    end
    local ok, result = pcall(narwhal.sql_run, "SELECT COUNT(*) FROM " .. quote_ident(name))
    if not ok then
        -- sql_run raised; the second return is the error message.
        return "rc failed: " .. tostring(result)
    end
    local cell = result.rows[1] and result.rows[1][1]
    return name .. ": " .. tostring(cell) .. " row(s)"
end)
