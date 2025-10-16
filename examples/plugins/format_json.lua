-- Result transform: pretty-print cells that look like JSON.
--
-- A hand-rolled minimal JSON parser + pretty-printer (pure Lua so the
-- script has zero external deps). Cells that don't parse as JSON pass
-- through unchanged.

local function is_space(c) return c == " " or c == "\t" or c == "\n" or c == "\r" end

local function skip_ws(s, i)
    while i <= #s and is_space(s:sub(i, i)) do i = i + 1 end
    return i
end

local parse_value
local function parse_string(s, i)
    if s:sub(i, i) ~= '"' then return nil, i end
    i = i + 1
    local out = {}
    while i <= #s do
        local c = s:sub(i, i)
        if c == '"' then return table.concat(out), i + 1 end
        if c == '\\' then
            local esc = s:sub(i + 1, i + 1)
            if esc == 'n' then out[#out + 1] = '\n'
            elseif esc == 't' then out[#out + 1] = '\t'
            elseif esc == '"' then out[#out + 1] = '"'
            elseif esc == '\\' then out[#out + 1] = '\\'
            else out[#out + 1] = esc end
            i = i + 2
        else
            out[#out + 1] = c
            i = i + 1
        end
    end
    return nil, i
end

local function parse_number(s, i)
    local start = i
    if s:sub(i, i) == '-' then i = i + 1 end
    while i <= #s and s:sub(i, i):match("[0-9.eE+-]") do i = i + 1 end
    local n = tonumber(s:sub(start, i - 1))
    if n == nil then return nil, start end
    return n, i
end

parse_value = function(s, i)
    i = skip_ws(s, i)
    if i > #s then return nil, i end
    local c = s:sub(i, i)
    if c == '"' then return parse_string(s, i) end
    if c == '{' then
        local obj, k, v = {}, nil, nil
        i = i + 1
        i = skip_ws(s, i)
        if s:sub(i, i) == '}' then return obj, i + 1 end
        while i <= #s do
            i = skip_ws(s, i)
            k, i = parse_string(s, i)
            if k == nil then return nil, i end
            i = skip_ws(s, i)
            if s:sub(i, i) ~= ':' then return nil, i end
            i = i + 1
            v, i = parse_value(s, i)
            if v == nil and s:sub(i, i) ~= 'n' then return nil, i end
            obj[k] = v
            i = skip_ws(s, i)
            local d = s:sub(i, i)
            if d == '}' then return obj, i + 1 end
            if d ~= ',' then return nil, i end
            i = i + 1
        end
        return nil, i
    end
    if c == '[' then
        local arr = {}
        i = i + 1
        i = skip_ws(s, i)
        if s:sub(i, i) == ']' then return arr, i + 1 end
        while i <= #s do
            local v
            v, i = parse_value(s, i)
            arr[#arr + 1] = v
            i = skip_ws(s, i)
            local d = s:sub(i, i)
            if d == ']' then return arr, i + 1 end
            if d ~= ',' then return nil, i end
            i = i + 1
        end
        return nil, i
    end
    if c == 't' and s:sub(i, i + 3) == 'true' then return true, i + 4 end
    if c == 'f' and s:sub(i, i + 4) == 'false' then return false, i + 5 end
    if c == 'n' and s:sub(i, i + 3) == 'null' then return nil, i + 4 end
    if c == '-' or (c >= '0' and c <= '9') then return parse_number(s, i) end
    return nil, i
end

local function pretty(v, indent)
    indent = indent or ""
    local next_indent = indent .. "  "
    local t = type(v)
    if t == "nil" then return "null" end
    if t == "boolean" then return tostring(v) end
    if t == "number" then return tostring(v) end
    if t == "string" then return '"' .. v:gsub('\\', '\\\\'):gsub('"', '\\"') .. '"' end
    if t == "table" then
        -- Array if every key is a positive integer with no gaps.
        local n, is_array = 0, true
        for k in pairs(v) do
            n = n + 1
            if type(k) ~= "number" then is_array = false end
        end
        if is_array and n > 0 then
            local parts = {}
            for _, item in ipairs(v) do parts[#parts + 1] = next_indent .. pretty(item, next_indent) end
            return "[\n" .. table.concat(parts, ",\n") .. "\n" .. indent .. "]"
        end
        if n == 0 then return is_array and "[]" or "{}" end
        local parts = {}
        for k, item in pairs(v) do
            parts[#parts + 1] = next_indent .. '"' .. k .. '": ' .. pretty(item, next_indent)
        end
        return "{\n" .. table.concat(parts, ",\n") .. "\n" .. indent .. "}"
    end
    return tostring(v)
end

local function try_pretty(cell)
    if type(cell) ~= "string" then return cell end
    local trimmed = cell:match("^%s*(.-)%s*$") or cell
    if trimmed == "" then return cell end
    local first = trimmed:sub(1, 1)
    if first ~= "{" and first ~= "[" then return cell end
    local value, _ = parse_value(trimmed, 1)
    if value == nil and trimmed ~= "null" then return cell end
    return pretty(value)
end

narwhal.register_transform(function(result)
    for _, row in ipairs(result.rows) do
        for i, cell in ipairs(row) do
            row[i] = try_pretty(cell)
        end
    end
end)
