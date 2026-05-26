# narwhal — TUI demo
#
# Render:   nix-shell -p vhs ffmpeg ttyd --run "vhs docs/img/demo.tape"
# Output:   docs/img/demo.gif
#
# Uses an isolated XDG_CONFIG_HOME so the demo doesn't touch the
# real user config. Sample DB at /tmp/narwhal-demo/blog.db is
# created by docs/img/seed-demo-db.sh.

Output docs/img/demo.gif

Set Shell      bash
Set FontSize   16
Set Width      1200
Set Height     720
Set Theme      "Catppuccin Mocha"
Set Padding    20
Set TypingSpeed 60ms
Set PlaybackSpeed 1.0

# Isolate the demo from the user's real narwhal config.
Hide
Type "export XDG_CONFIG_HOME=/tmp/narwhal-demo/config"
Enter
Type "clear"
Enter
Show

# Launch the TUI.
Type "narwhal"
Sleep 500ms
Enter
Sleep 2s

# Open the saved connection.
Type ":open blog"
Sleep 500ms
Enter
Sleep 1500ms

# Compose a query (vim insert mode).
Type "i"
Sleep 300ms
Type "SELECT a.name, p.title, p.views"
Enter
Type "FROM posts p"
Enter
Type "JOIN authors a ON a.id = p.author_id"
Enter
Type "ORDER BY p.views DESC;"
Sleep 500ms

# Back to normal mode, run the whole buffer via the palette.
Escape
Sleep 400ms
Type ":run"
Enter
Sleep 2s

# Browse results.
Down@200ms 3
Sleep 800ms

# Wipe the buffer for a second query.
Type "ggVGd"
Sleep 300ms
Type "i"
Type "SELECT title, tags, views FROM posts WHERE views > 1000;"
Escape
Sleep 400ms
Type ":run"
Enter
Sleep 1500ms

# Jump cursor to the tags cell and pop the JSON viewer.
Down@150ms 2
Right@150ms 2
Type "z"
Sleep 2s
Type "q"
Sleep 500ms

# End with the help overlay so viewers see the keymap surface.
Type "?"
Sleep 2500ms
Escape
Sleep 600ms

# Quit cleanly.
Type ":quit"
Enter
Sleep 800ms
