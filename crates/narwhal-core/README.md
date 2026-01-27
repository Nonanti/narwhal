# narwhal-core

Driver trait, value model, schema types and the workspace `Error` enum.

Every other crate sees the world through `narwhal-core`. It defines `DatabaseDriver`, `Connection`, `Row`, `Value`, `ColumnHeader`, `Schema`, `Table` and the shared `Error` / `Result` aliases.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
