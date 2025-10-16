-- :top <table>  -  inject 'SELECT * FROM <table> LIMIT 10' into the editor
--
-- Cheapest possible "snippet expander" plugin. Useful as a template
-- for your own daily commands — every shape you'd type ten times a day
-- becomes one line of Lua.

narwhal.register_command("top", "SELECT * FROM <table> LIMIT 10", function(arg)
    local name = arg:match("^%s*(.-)%s*$")
    if name == "" then
        return "top: table name required"
    end
    return {
        sql = "SELECT * FROM " .. name .. " LIMIT 10;\n",
        append = true,
    }
end)

narwhal.register_command("desc", "show schema for <table> via information_schema", function(arg)
    local name = arg:match("^%s*(.-)%s*$")
    if name == "" then
        return "desc: table name required"
    end
    return {
        sql = string.format(
            "SELECT column_name, data_type, is_nullable\n" ..
            "  FROM information_schema.columns\n" ..
            " WHERE table_name = '%s'\n" ..
            " ORDER BY ordinal_position;\n",
            name
        ),
        append = true,
    }
end)
