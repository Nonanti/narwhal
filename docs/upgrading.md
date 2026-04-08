# Upgrading

## v1.x → v2.0

narwhal 2.0 ships a v2 schema for `connections.toml` and `config.toml`.
On first launch against a v1 file you'll see a warning. Run:

```sh
narwhal migrate-config         # writes v2 in place, keeps .v1.bak
narwhal config validate        # dry-run check
```

The migration is idempotent and reversible — the original file is
preserved as `.v1.bak` until you delete it manually.

See [`CHANGELOG.md`](../CHANGELOG.md) for the full breaking-change
list and rationale.

## v2.0 → v2.1

No schema change. Just upgrade the binary:

```sh
curl -fsSL https://github.com/Nonanti/narwhal/releases/latest/download/install.sh | NARWHAL_FORCE=1 sh
```

Highlights:

- Three editor modes (vim / basic / emacs) + full mouse support
- In-app `:settings` modal with live config reload
- Prebuilt Linux binaries are now self-contained (libdbus vendored)

Full notes: [`CHANGELOG.md`](../CHANGELOG.md#210---2026-06-08).
