-- Result transform: uppercase every TEXT cell.
--
-- Smoke test that confirms the transform hook is wired up on your
-- install. Drop it into ~/.config/narwhal/plugins/ and any query that
-- returns text will come back shouted.

narwhal.register_transform(function(result)
    for _, row in ipairs(result.rows) do
        for i, cell in ipairs(row) do
            if type(cell) == "string" then
                row[i] = cell:upper()
            end
        end
    end
end)
