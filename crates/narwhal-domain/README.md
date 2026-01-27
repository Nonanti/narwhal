# narwhal-domain

Pure domain models. No IO, no rendering, no async.

Holds `EditorBuffer` and its support types, the `SchemaListing` alias, and (incrementally) the rest of the application's view-state. Hosts consume by reference and route mutations through the published API.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
