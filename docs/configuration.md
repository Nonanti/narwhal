# Configuration

narwhal reads two TOML files from `~/.config/narwhal/`:

| File | Purpose |
|---|---|
| `connections.toml` | Database connections (per-host credentials, SSL, SSH tunnels) |
| `config.toml`      | Editor, theme, keybindings, diagrams, plugins |

Both are watched live — saving from another editor reloads narwhal
without a restart.

## Connections

```toml
[[connection]]
id     = "00000000-0000-0000-0000-000000000001"
name   = "prod-pg"
driver = "postgres"
color  = "red"           # status bar tint
read_only      = true    # syntactic write guard
confirm_writes = true    # type YES before any mutation

[connection.params]
host     = "db.example.com"
port     = 5432
username = "alice"
password = "${vault:secret/prod/db-password}"   # or use keyring
database = "app"
ssl_mode = "verify-full"
```

Password sources, in priority order:

1. `${vault:…}` — HashiCorp Vault. See [`vault.md`](./vault.md).
2. `${op:…}` — 1Password CLI.
3. OS keyring (Secret Service, Keychain, Credential Manager).
4. `~/.pgpass` (Postgres only).
5. Inline plaintext (development only — narwhal warns).

## Settings

```toml
theme = "dark"           # dark | light | high-contrast

[editor]
mode         = "vim"     # vim | basic | emacs
mouse        = "enabled" # enabled | click-only | disabled
tab_width    = 4
line_numbers = true

[keybindings]
preset = "default"       # default | vscode | datagrip | intellij

[diagram]
icons = "ascii"          # ascii | nerdfont
```

See also:

- [`editor-modes.md`](./editor-modes.md) — vim / basic / emacs detail
- [`mouse.md`](./mouse.md) — mouse vocabulary and gestures
- [`settings.md`](./settings.md) — every key with defaults
- [`vault.md`](./vault.md) — secret backends
