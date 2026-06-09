#!/usr/bin/env bash
# narwhal MCP demo -- simulates an agent using narwhal as its database client.
# Each tool call is its own short-lived `narwhal mcp` invocation, exactly the
# way the protocol is meant to run.

set -u

# Repo-root-relative binary path. Override with NARWHAL_BIN if needed.
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
NARWHAL_BIN="${NARWHAL_BIN:-$REPO_ROOT/target/release/narwhal}"

# ANSI helpers
BOLD=$'\e[1m'; DIM=$'\e[2m'; RESET=$'\e[0m'
CYAN=$'\e[36m'; MAGENTA=$'\e[35m'; GREEN=$'\e[32m'; YELLOW=$'\e[33m'; BLUE=$'\e[34m'; GREY=$'\e[90m'

# Pacing (overridable from env so the demo can be replayed instantly for QA)
SLOW=${SLOW:-0.028}
PAUSE_SHORT=${PAUSE_SHORT:-0.4}
PAUSE_LONG=${PAUSE_LONG:-0.9}

# Type a string out one character at a time.
type_out() {
    local s="$1"
    local i ch
    for ((i=0; i<${#s}; i++)); do
        ch="${s:i:1}"
        printf '%s' "$ch"
        [ "$SLOW" != "0" ] && sleep "$SLOW"
    done
    printf '\n'
}

banner() {
    printf '\n %s%s%s%s\n\n' "$BOLD" "$BLUE" "$1" "$RESET"
}

user_msg() {
    printf '%s%s you  %s ' "$BOLD" "$MAGENTA" "$RESET"
    type_out "$1"
    sleep "$PAUSE_SHORT"
}

agent_msg() {
    printf '%s%s agent%s ' "$BOLD" "$CYAN" "$RESET"
    type_out "$1"
    sleep "$PAUSE_SHORT"
}

agent_thinks() {
    printf '%s      ...%s %s\n' "$GREY" "$RESET" "$1"
    sleep "$PAUSE_SHORT"
}

tool_call() {
    local name="$1"; local args="$2"
    printf '%s%s -> tool %s%s %s%s\n' "$BOLD" "$YELLOW" "$RESET" "$BOLD" "$name" "$RESET"
    printf '%s        args: %s%s\n' "$GREY" "$args" "$RESET"
}

# tool_invoke <name> <json_args>  -> prints JSON result text
tool_invoke() {
    local name="$1" args="$2"
    {
        printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"demo-agent","version":"0.1"}}}'
        printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
        printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"%s","arguments":%s}}\n' "$name" "$args"
    } | "$NARWHAL_BIN" mcp --read-only 2>/dev/null \
      | jq -r 'select(.id == 2) | .result.content[0].text'
}

clear
banner "narwhal MCP -- your agent now speaks SQL natively"
printf '  %sfaster than a REPL, safer than handing over a shell.%s\n\n' "$DIM" "$RESET"
sleep 1.0

# 1. user asks
user_msg "which region drove the most revenue this week, and which product carried it?"
sleep "$PAUSE_LONG"

# 2. discover connections
agent_thinks "let me see which databases I can reach."
tool_call "list_connections" "{}"
RESP=$(tool_invoke "list_connections" '{}')
printf '%s' "$RESP" | jq -C '.connections' | sed 's/^/        /'
sleep "$PAUSE_LONG"

# 3. introspect schema
agent_thinks "store database. let me read its schema."
tool_call "describe_schema" '{"connection":"store"}'
RESP=$(tool_invoke "describe_schema" '{"connection":"store"}')
printf '%s' "$RESP" | jq -C '.schemas[0].tables | map(.name)' | sed 's/^/        /'
sleep "$PAUSE_LONG"

# 4. revenue by region
agent_thinks "orders x customers gives revenue by region. paid only."
SQL_REGION='SELECT c.region, COUNT(*) AS orders, ROUND(SUM(o.total_cents)/100.0, 2) AS revenue_usd FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.status = '"'"'paid'"'"' GROUP BY c.region ORDER BY revenue_usd DESC'
tool_call "run_query" '{"connection":"store","sql":"SELECT region, COUNT(*), SUM(...) GROUP BY region ..."}'
ARGS=$(jq -nc --arg sql "$SQL_REGION" '{connection:"store", sql:$sql}')
RESP=$(tool_invoke "run_query" "$ARGS")
printf '%s' "$RESP" | jq -C '{columns: (.columns | map(.name)), rows, elapsed_ms, read_only}' | sed 's/^/        /'
sleep "$PAUSE_LONG"

# 5. top product in winning region
agent_thinks "US won. which product carried the revenue?"
SQL_PROD='SELECT p.name, SUM(oi.qty) AS units, ROUND(SUM(oi.qty * p.price_cents)/100.0, 2) AS revenue_usd FROM order_items oi JOIN orders o ON o.id = oi.order_id JOIN customers c ON c.id = o.customer_id JOIN products p ON p.id = oi.product_id WHERE o.status = '"'"'paid'"'"' AND c.region = '"'"'US'"'"' GROUP BY p.name ORDER BY revenue_usd DESC LIMIT 3'
tool_call "run_query" '{"connection":"store","sql":"SELECT p.name, SUM(oi.qty) ... WHERE c.region = '"'"'US'"'"' LIMIT 3"}'
ARGS=$(jq -nc --arg sql "$SQL_PROD" '{connection:"store", sql:$sql}')
RESP=$(tool_invoke "run_query" "$ARGS")
printf '%s' "$RESP" | jq -C '.rows' | sed 's/^/        /'
sleep "$PAUSE_LONG"

# 6. final answer
echo
agent_msg "US led the week at \$1,596 across 3 paid orders (EU: \$763 across 2)."
agent_msg "the 27\" 4K monitor carried it -- 2 units, \$998 -- ahead of headphones and the keyboard."
sleep "$PAUSE_LONG"

# 7. closing card
echo
printf '%s%s every call: sandboxed, read-only, ROLLBACK-wrapped.%s\n' "$BOLD" "$GREEN" "$RESET"
printf '%s     narwhal mcp -- one binary, six engines, zero glue code.%s\n' "$DIM" "$RESET"
echo
sleep 2.5
