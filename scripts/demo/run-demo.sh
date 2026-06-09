#!/usr/bin/env bash
# Wrapper that exports the isolated XDG dirs and invokes the agent demo.
# Run scripts/demo/setup-store-db.sh first.
set -u
ROOT="${NARWHAL_DEMO_ROOT:-/tmp/narwhal-demo}"
export XDG_CONFIG_HOME="$ROOT/config"
export XDG_DATA_HOME="$ROOT/data"
export XDG_CACHE_HOME="$ROOT/cache"
HERE="$(cd "$(dirname "$0")" && pwd)"
exec "$HERE/agent-demo.sh"
