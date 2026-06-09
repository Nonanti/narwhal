# narwhal MCP demo — VHS tape
#
# Render with:
#   vhs docs/img/demo-mcp.tape
#
# Produces docs/img/demo-mcp.gif. The demo runs a self-contained
# /tmp/narwhal-demo setup and simulates an agent driving narwhal MCP
# end-to-end (list_connections -> describe_schema -> run_query).

Output docs/img/demo-mcp.gif

# Terminal styling
Set Shell        "bash"
Set FontSize     16
Set FontFamily   "FiraCode Nerd Font Mono"
Set Width        1280
Set Height       780
Set Padding      24
Set Theme        "Catppuccin Mocha"
Set TypingSpeed  50ms
Set PlaybackSpeed 1.0

# Quiet bash prompt during recording.
Hide
Type `export PS1='$ '` Enter
Type `clear` Enter
Show

# Run the demo. The wrapper exports the isolated XDG_*_HOME paths and
# execs scripts/demo/agent-demo.sh. Run scripts/demo/setup-store-db.sh
# first to prepare /tmp/narwhal-demo/.
Type `./scripts/demo/run-demo.sh` Sleep 500ms Enter

# The script itself paces with sleeps; just let it run to completion.
# The agent transcript is ~25s long; pad a little after the closing card.
Sleep 28s
