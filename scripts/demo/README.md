# narwhal MCP demo

Sources for `docs/img/demo-mcp.gif` — the recording that shows an
agent driving narwhal's MCP server end-to-end.

The demo is fully reproducible: it runs against a temporary SQLite
database under `/tmp/narwhal-demo/`, with an isolated `XDG_CONFIG_HOME`
so your real `~/.config/narwhal` is untouched.

## Layout

| File | Purpose |
| --- | --- |
| `setup-store-db.sh` | Builds the SQLite store + isolated config in `/tmp/narwhal-demo/`. |
| `agent-demo.sh` | Bash "agent simulator". Speaks JSON-RPC to `narwhal mcp` and pretty-prints the conversation. |
| `run-demo.sh` | Wrapper: exports the isolated `XDG_*_HOME` paths and execs `agent-demo.sh`. |
| `../../docs/img/demo-mcp.tape` | VHS recording script consumed by `vhs`. |
| `../../docs/img/demo-mcp.gif` | Rendered output. |

## Reproduce locally

```sh
# 1. build release binary (the demo uses target/release/narwhal)
cargo build --release --bin narwhal

# 2. prepare the demo workspace
./scripts/demo/setup-store-db.sh

# 3. (optional) preview the demo in your terminal
./scripts/demo/run-demo.sh

# 4. re-render the GIF
vhs docs/img/demo-mcp.tape
```

Requirements: `bash`, `jq`, `sqlite3`, `vhs` (which pulls in `ttyd`
and `ffmpeg`). A monospaced font with reasonable Unicode coverage is
recommended; the tape uses `FiraCode Nerd Font Mono`.

## Pacing knobs

`agent-demo.sh` reads three env vars:

- `SLOW` — per-character typing delay (default `0.028`s)
- `PAUSE_SHORT` — short pause between sections (default `0.4`s)
- `PAUSE_LONG` — long pause before agent decisions (default `0.9`s)

Set all three to `0` for instant playback when QA-testing the script:

```sh
SLOW=0 PAUSE_SHORT=0 PAUSE_LONG=0 ./scripts/demo/run-demo.sh
```
