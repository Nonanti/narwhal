# narwhal-app

Application runtime and event loop.

Wires the driver registry, configuration, modal input and terminal UI together. Owns `AppCore`, the channels, the draw scheduler and the plugin host. Other crates do not depend on this one.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
