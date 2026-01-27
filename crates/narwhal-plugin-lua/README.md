# narwhal-plugin-lua

Lua scripting runtime for narwhal plugins.

Each `LuaPlugin` owns a single `mlua::Lua` state. Depends only on `narwhal-plugin`; the contract surface is the plugin API, never the application internals.

Part of the [narwhal](../../README.md) workspace. See [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md) for the layered crate map.
