-- :explain-cost   -  wrap the editor buffer in EXPLAIN ANALYZE
-- :explain-sqlite -  wrap the editor buffer in plain EXPLAIN (SQLite)
--
-- Extremely common daily action — saves re-typing the prefix every
-- time you want to inspect a query plan.  The Postgres variant uses
-- EXPLAIN ANALYZE for actual execution cost; the SQLite variant
-- falls back to plain EXPLAIN (SQLite does not support ANALYZE in
-- this context).
--
-- The plugin reads the current editor buffer via narwhal.editor_text,
-- prepends the EXPLAIN prefix, and injects the wrapped statement
-- back with append=false so the buffer is replaced.
--
-- Example:
--   (type a SELECT into the editor)
--   :explain-cost
--   -> editor now contains: EXPLAIN ANALYZE <your SELECT>
--
-- ⚠️ If the buffer is empty, the plugin reports a hint rather than
-- injecting a bare EXPLAIN ANALYZE.

narwhal.register_command("explain-cost", "wrap editor buffer in EXPLAIN ANALYZE", function(_)
    local text = narwhal.editor_text or ""
    local trimmed = text:match("^%s*(.-)%s*$") or ""
    if trimmed == "" then
        return "explain-cost: editor is empty; type a statement first"
    end
    return {
        sql = "EXPLAIN ANALYZE " .. trimmed .. "\n",
        append = false,
    }
end)

narwhal.register_command("explain-sqlite", "wrap editor buffer in EXPLAIN (SQLite)", function(_)
    local text = narwhal.editor_text or ""
    local trimmed = text:match("^%s*(.-)%s*$") or ""
    if trimmed == "" then
        return "explain-sqlite: editor is empty; type a statement first"
    end
    return {
        sql = "EXPLAIN " .. trimmed .. "\n",
        append = false,
    }
end)
