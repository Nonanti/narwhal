//! Stateless command and helper modules for narwhal.
//!
//! Each module here is a self-contained piece of the host application
//! that does not own runtime state: completion engine, export pipeline,
//! connection wizard, snippet store, DDL/EXPLAIN helpers, inline cell
//! editing, statement extraction and the `:`-prompt command dispatch.
//!
//! Hosts (`narwhal-app`, the headless CLI, the MCP server) call into
//! these modules with the data they own; nothing here reaches back into
//! the host.

#![forbid(unsafe_code)]

pub mod action;
pub mod cell_edit;
pub mod commands;
pub mod completion;
pub mod ddl;
pub mod explain;
pub mod export;
pub mod keymap;
pub mod meta;
pub mod pending;
pub mod pre_connect;
pub mod schema_diff;
pub mod session;
pub mod snippets;
pub mod statements;
pub mod template;
pub mod wizard;
