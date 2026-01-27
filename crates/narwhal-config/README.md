# narwhal-config

On-disk configuration and credential storage.

Owns `ConfigPaths`, `ConnectionsFile`, `Settings`, `LastUsedStore`, the `pgpass` parser, and the `KeyringStore` / `InMemoryStore` credential backends.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
