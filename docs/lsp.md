# LSP integration

narwhal v2.0 ships an embedded LSP client in the
[`narwhal-lsp`](../crates/narwhal-lsp) crate. It speaks JSON-RPC 2.0
over a generic `Transport` and is wired for two upstream servers out
of the box: `sqls` and `sqlls`.

## Status

- ✅ Crate published as a workspace member.
- ✅ `Transport` (Content-Length framing) implemented for both child
  process stdio and an in-memory test double.
- ✅ `Client` lifecycle: `initialize` / `initialized` / `shutdown` /
  `exit`, plus typed helpers for `textDocument/didOpen`,
  `textDocument/didChange`, `textDocument/completion`,
  `textDocument/hover`.
- ✅ `[settings.lsp]` schema in `~/.config/narwhal/settings.toml`.
- ⏳ Editor pane wiring (popup, debounce, cancellation) is deferred
  to v2.1.
- ⏳ Schema-aware connection scoping (rewriting sqls's config on
  `:open <conn>`) is deferred to v2.1.

The protocol layer is exhaustively tested against an in-memory
transport so the v2.1 wiring task can focus on the editor-pane half
without re-litigating framing or request/response routing.

## Configuration

Add the following to `~/.config/narwhal/settings.toml`. v2.0 reads
the section so a user can author it ahead of the v2.1 wiring; the
client is not yet spawned at startup.

```toml
[settings.lsp]
enabled = false
server = "sqls"  # "sqls" | "sqlls" | /path/to/binary
config_file = ""  # leave empty to let the server use its own default
```

## Installing the upstream server

### sqls (recommended)

```bash
go install github.com/sqls-server/sqls@latest
```

Configure connections in `~/.config/sqls/config.yml`.

### sqlls

```bash
npm install -g sql-language-server
```

The narwhal client passes `--stdio` automatically.

## Roadmap (v2.1)

- Wire `Client::completion` into the editor's Ctrl-Space chord.
- Render `textDocument/hover` results in a transient popup
  triggered by `Ctrl-K`.
- Implement `textDocument/definition` jump (chord TBD).
- Graceful degradation: server crash → 3 restart attempts → status
  bar message.
- Schema-aware connection scoping: rewrite the server's config file
  on every `:open <conn>` and restart.
