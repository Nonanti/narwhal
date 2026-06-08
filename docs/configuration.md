# Configuration

narwhal reads two TOML files from `~/.config/narwhal/` (or the
platform equivalent: `$XDG_CONFIG_HOME/narwhal/` on Linux,
`~/Library/Application Support/narwhal/` on macOS).

- `config.toml` — editor, theme, audit, vault, plugin, LSP settings.
- `connections.toml` — saved database connections.

Both are created on first launch with sensible defaults.

## `config.toml`

A minimal file:

```toml
[editor]
mode = "vim"          # vim | basic | emacs
mouse = "enabled"     # enabled | click-only | disabled
theme = "default"
show_line_numbers = true
show_mode_indicator = true

[keybindings]
preset = "default"    # default | vscode | datagrip | intellij

[run]
batch_size = 64
flush_window_ms = 50
```

Open the in-app editor with `:settings` — changes save atomically
and reload live within ~50 ms.

### Theme

```toml
[editor]
theme = "default"     # default | dark | light | solarized
```

### Audit

```toml
[[audit.sinks]]
kind = "file"
path = "~/.local/share/narwhal/audit.log"
rotate = "daily"
```

See [`audit.md`](./audit.md) for the JSONL schema and sink options.

### Vault

```toml
[vault]
default = "main"

[[vault.providers]]
name = "main"
kind = "hashicorp"
address = "https://vault.internal"
auth = "token"
```

See [`vault.md`](./vault.md) for `1password`, AWS, and Azure
backends.

### Plugins

```toml
[plugins]
auto_load = true
plugins_dir = "~/.config/narwhal/plugins"
```

See [`plugins.md`](./plugins.md) for the Lua and WASM runtimes.

### LSP

```toml
[lsp]
enabled = false
server = "sqls"       # sqls | sqlls
```

## `connections.toml`

```toml
[[connection]]
name = "prod"
url  = "postgres://app@db.internal/orders"
read_only = true
confirm_writes = false
color = "red"

[[connection]]
name = "local"
url  = "sqlite:./scratch.db"
```

Add a connection interactively with `:add` inside the TUI, or
import from a URL with `:url <dsn>`.

### Environment interpolation

Values support `${env:NAME}` and `${env:NAME:fallback}`:

```toml
[[connection]]
name = "staging"
url  = "postgres://app@${env:DB_HOST}/${env:DB_NAME:orders}"
```

Missing variables fail at startup with a clear error.

### Pre-connect commands

Each connection can run shell steps before the driver opens. Useful
for short-lived credentials.

```toml
[[connection]]
name = "iam"
url  = "postgres://app@db.aws.example/orders"

[[connection.pre_connect]]
command = "aws rds generate-db-auth-token --hostname ${env:DB_HOST} --port 5432 --username app"
save_output_to = "DB_PASSWORD"
timeout_secs = 30
```

The captured stdout is exposed to later params via
`${preconnect:NAME}`.

## Workspace state

Open tabs, cursor positions, and sidebar expansion restore across
launches via `~/.local/state/narwhal/workspace-state.toml`. Disable
in `config.toml`:

```toml
[workspace]
persist = false
```

## Reference

| File                              | Purpose                                 |
|-----------------------------------|-----------------------------------------|
| `~/.config/narwhal/config.toml`   | Settings (editor, audit, vault, …)      |
| `~/.config/narwhal/connections.toml` | Saved connections                    |
| `~/.config/narwhal/plugins/`      | User-installed plugins                  |
| `~/.local/state/narwhal/`         | Workspace state, history, audit logs    |
