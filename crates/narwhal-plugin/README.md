# narwhal-plugin

Plugin trait + registry.

Defines the stable contract plugin runtimes implement: command handlers and result transforms. Hosts hold a `Vec<Arc<dyn Plugin>>` and route by name.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
