-- :csv-export <table> <path>  -  dump a table to a CSV file
--
-- Queries the given table via narwhal.sql_run and writes the result
-- to <path> as comma-separated values. The first line is the column
-- header; every subsequent line is one data row. Cells that contain
-- a comma, double-quote or newline are quoted with the standard CSV
-- doubling rule (RFC 4180).
--
-- An empty result set is valid output (just the header line).
-- Paths containing ".." are rejected to avoid accidental writes
-- outside the working directory.
--
-- Example:
--   :open mydb
--   :csv-export items /tmp/items.csv
--   -> status bar: wrote 3 row(s) to /tmp/items.csv
--
-- ⚠️ SQL injection: the table name is whitelist-validated before it
-- is spliced into the SELECT statement — see safe_ident below.

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

-- Escape a single cell for CSV output per RFC 4180.
-- NULL cells arrive from narwhal.sql_run as Lua nil. Emit an empty
-- field for those instead of the literal string "nil" — round-tripping
-- the CSV back through Pandas/Excel would otherwise materialise "nil"
-- as a real string value and corrupt the data.
local function csv_escape(cell)
    if cell == nil then return "" end
    local s = tostring(cell)
    if s:find('[,"\r\n]') then
        return '"' .. s:gsub('"', '""') .. '"'
    end
    return s
end

narwhal.register_command("csv-export", "export <table> to <path> as CSV", function(arg)
    -- Parse "<table> <path>" — split on the first whitespace, the
    -- rest (including spaces) is the file path.
    local raw = arg:match("^%s*(.-)%s*$")
    if raw == "" then
        return "csv-export: usage :csv-export <table> <path>"
    end
    local name_str, path = raw:match("^(%S+)%s+(.+)$")
    if name_str == nil or path == nil or path == "" then
        return "csv-export: usage :csv-export <table> <path>"
    end

    -- Path validation: refuse ".." to avoid writing outside cwd.
    if path:find("%.%.") then
        return "csv-export: path must not contain '..'"
    end

    local name, err = safe_ident(name_str)
    if name == nil then
        return "csv-export: " .. err
    end

    local ok, result = pcall(narwhal.sql_run, "SELECT * FROM " .. quote_ident(name))
    if not ok then
        return "csv-export failed: " .. tostring(result)
    end

    -- Build CSV content.
    local lines = {}

    -- Header row.
    local headers = {}
    for _, col in ipairs(result.columns) do
        headers[#headers + 1] = csv_escape(col.name)
    end
    lines[#lines + 1] = table.concat(headers, ",")

    -- Data rows.
    for _, row in ipairs(result.rows) do
        local cells = {}
        for _, cell in ipairs(row) do
            cells[#cells + 1] = csv_escape(cell)
        end
        lines[#lines + 1] = table.concat(cells, ",")
    end

    -- Write to disk.
    local file, io_err = io.open(path, "w")
    if file == nil then
        return "csv-export: cannot open " .. path .. ": " .. tostring(io_err)
    end
    file:write(table.concat(lines, "\n") .. "\n")
    file:close()

    local row_count = #result.rows
    return "wrote " .. row_count .. " row(s) to " .. path
end)
