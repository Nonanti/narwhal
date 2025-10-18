-- :top <table>  -  inject 'SELECT * FROM <table> LIMIT 10' into the editor
-- :desc <table> -  inject information_schema describe boilerplate
--
-- Cheapest possible "snippet expander" plugin. Useful as a template
-- for your own daily commands.
--
-- ⚠️ Both commands embed the user's argument into a SQL string. We
-- whitelist the name to letters/digits/underscore before splicing —
-- the editor would happily accept ':top "; DROP TABLE x;--"' and
-- store the malicious text waiting for you to hit Run. Don't ship a
-- snippet plugin without input validation.

local function safe_ident(s)
    if s == nil then return nil, "table name required" end
    if s:match("^[%a_][%w_]*$") == nil then
        return nil, ("invalid table name '%s' (letters, digits, _ only)"):format(s)
    end
    return s
end

narwhal.register_command("top", "SELECT * FROM <table> LIMIT 10", function(arg)
    local raw = arg:match("^%s*(.-)%s*$")
    local name, err = safe_ident(raw)
    if name == nil then
        return "top: " .. err
    end
    return {
        sql = "SELECT * FROM " .. name .. " LIMIT 10;\n",
        append = true,
    }
end)

narwhal.register_command("desc", "show schema for <table> via information_schema", function(arg)
    local raw = arg:match("^%s*(.-)%s*$")
    local name, err = safe_ident(raw)
    if name == nil then
        return "desc: " .. err
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
