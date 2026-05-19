/// Selector for [`Command::DumpSchema`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DumpTarget {
    /// Dump the table currently shown in the result pane (TableDetail).
    Current,
    /// Dump every table the active session knows about.
    All,
    /// Dump the named table (resolved through the active session).
    Named(String),
}

/// Isolation levels accepted by `:begin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationArg {
    ReadUncommitted,
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

impl IsolationArg {
    pub fn parse(token: &str) -> Option<Self> {
        match token
            .to_ascii_lowercase()
            .replace([' ', '_', '-'], "")
            .as_str()
        {
            "readuncommitted" | "uncommitted" | "ru" => Some(Self::ReadUncommitted),
            "readcommitted" | "committed" | "rc" => Some(Self::ReadCommitted),
            "repeatableread" | "repeatable" | "rr" => Some(Self::RepeatableRead),
            "serializable" | "s" => Some(Self::Serializable),
            _ => None,
        }
    }
}

/// Top-level `:`-line commands accepted by the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Quit,
    Open(String),
    Close,
    Refresh,
    Run,
    RunAll,
    Stream,
    StreamAll,
    Cancel,
    Clear,
    Explain,
    Export {
        format: String,
        path: String,
    },
    DumpSchema {
        target: DumpTarget,
    },
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    Add,
    Begin(Option<IsolationArg>),
    /// Re-run the most recent table preview with the next page of rows.
    NextPage,
    /// Re-run the most recent table preview with the previous page.
    PrevPage,
    /// Set the page size used by [`Command::NextPage`] / [`Command::PrevPage`]
    /// and the initial sidebar preview.
    PageSize(usize),
    Commit,
    Rollback,
    Savepoint(String),
    Release(String),
    RollbackTo(String),
    /// Remove a saved connection by name (also clears its keyring entry).
    Remove(String),
    /// Forget the keyring password for a saved connection by name; the
    /// connection itself stays in `connections.toml`.
    Forget(String),
    /// Load a Lua plugin from disk (`:plug-load path/to/foo.lua`).
    PluginLoad(String),
    /// List loaded plugins and the commands they expose.
    PluginList,
    /// Open the Ctrl+R history modal.
    History,
    Help(Option<String>),
    /// Substitute command: `:s/old/new/[g][c]` or `:%s/old/new/[g][c]`.
    Substitute {
        range: SubstituteRange,
        pattern: String,
        replacement: String,
        global: bool,
        confirm: bool,
    },
    /// Clear search highlighting (`:nohlsearch`).
    NoHlSearch,
    Unknown(String),
    Empty,
}

/// Scope of a substitute command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstituteRange {
    /// Replace on the current line only (`:s/…`).
    CurrentLine,
    /// Replace across the entire buffer (`:%s/…`).
    WholeBuffer,
}

/// Every token the parser accepts as a built-in `:`-line command head.
/// Plugins that try to register one of these names are rejected at
/// load time so the user isn't left wondering why their `:run`
/// override never runs (the parser would always match the built-in
/// first).
///
/// Keep this list in sync with the `match head` arms below in [`parse`].
pub const BUILTIN_COMMAND_NAMES: &[&str] = &[
    "q",
    "quit",
    "exit",
    "open",
    "o",
    "close",
    "refresh",
    "r",
    "run",
    "run-all",
    "runall",
    "stream",
    "stream-all",
    "streamall",
    "cancel",
    "clear",
    "explain",
    "export",
    "dump-schema",
    "dumpschema",
    "add",
    "next",
    "next-page",
    "npage",
    "prev",
    "prev-page",
    "ppage",
    "page-size",
    "pagesize",
    "begin",
    "start",
    "commit",
    "rollback",
    "abort",
    "savepoint",
    "sp",
    "release",
    "rollback-to",
    "rollbackto",
    "remove",
    "rm",
    "forget",
    "plug-load",
    "plugload",
    "plug",
    "plug-list",
    "pluglist",
    "plugins",
    "history",
    "new",
    "tabnew",
    "tabclose",
    "tc",
    "tabnext",
    "tn",
    "tabprev",
    "tp",
    "tabprevious",
    "help",
    "h",
    "nohlsearch",
    "noh",
];

/// Short descriptions for built-in commands, looked up by `:help <name>`.
/// Each entry maps a primary token to a one-line human-readable summary.
/// Aliases (e.g. `"o"` for `"open"`) are not listed here — `:help o`
/// resolves through the parser to `Help(Some("o"))` and the core maps
/// aliases back to the primary key before consulting this table.
pub const BUILTIN_COMMAND_DESCRIPTIONS: &[(&str, &str)] = &[
    ("quit", "quit narwhal (also :q, :exit)"),
    ("open", "open a saved connection by name or URL (also :o)"),
    ("close", "close the current database connection"),
    (
        "refresh",
        "re-fetch the schema tree for the active connection",
    ),
    ("run", "execute the SQL statement under the cursor"),
    (
        "run-all",
        "execute every statement in the editor buffer (also :runall)",
    ),
    (
        "stream",
        "stream the SQL statement under the cursor (row by row)",
    ),
    (
        "stream-all",
        "stream every statement in the editor buffer (also :streamall)",
    ),
    ("cancel", "cancel the currently running query"),
    ("clear", "erase the editor buffer and its result"),
    (
        "explain",
        "run EXPLAIN ANALYZE on the current statement (postgres)",
    ),
    (
        "export",
        "export the current result to a file (:export csv|json|insert <path>)",
    ),
    (
        "dump-schema",
        "write CREATE TABLE DDL into the editor (:dump-schema [name|all])",
    ),
    ("add", "open the connection wizard to save a new connection"),
    (
        "next-page",
        "show the next page of the current table preview (also :next)",
    ),
    (
        "prev-page",
        "show the previous page of the current table preview (also :prev)",
    ),
    (
        "page-size",
        "set the number of rows per page for previews (:page-size N)",
    ),
    (
        "begin",
        "start a transaction with an optional isolation level",
    ),
    ("commit", "commit the open transaction"),
    ("rollback", "roll back the open transaction (also :abort)"),
    (
        "savepoint",
        "create a savepoint inside the open transaction (also :sp)",
    ),
    ("release", "release a previously created savepoint"),
    (
        "rollback-to",
        "roll back to a previously created savepoint (also :rollbackto)",
    ),
    ("remove", "remove a saved connection by name (also :rm)"),
    (
        "forget",
        "delete the stored password for a saved connection",
    ),
    (
        "plug-load",
        "load a Lua plugin from disk (:plug-load <path>)",
    ),
    (
        "plug-list",
        "list loaded plugins and the commands they expose",
    ),
    ("history", "open the query history modal (also Ctrl+R)"),
    ("new", "open a new editor tab (also :tabnew)"),
    ("tabclose", "close the current editor tab (also :tc)"),
    ("tabnext", "switch to the next editor tab (also :tn)"),
    ("tabprev", "switch to the previous editor tab (also :tp)"),
    (
        "help",
        "show help; :help <command> for details on a specific command",
    ),
    (
        "nohlsearch",
        "clear search highlighting in the editor (also :noh)",
    ),
];

/// Map an alias token back to its primary command key so that `:help o`
/// resolves to the description for "open", not "o".
pub fn resolve_builtin_alias(token: &str) -> &str {
    match token {
        "q" | "exit" => "quit",
        "o" => "open",
        "r" => "refresh",
        "runall" => "run-all",
        "streamall" => "stream-all",
        "dumpschema" => "dump-schema",
        "next" | "npage" => "next-page",
        "prev" | "ppage" => "prev-page",
        "pagesize" => "page-size",
        "start" => "begin",
        "abort" => "rollback",
        "sp" => "savepoint",
        "rollbackto" => "rollback-to",
        "rm" => "remove",
        "plugload" | "plug" => "plug-load",
        "pluglist" | "plugins" => "plug-list",
        "history" => "history",
        "tabnew" => "new",
        "tc" => "tabclose",
        "tn" => "tabnext",
        "tp" | "tabprevious" => "tabprev",
        "h" => "help",
        "noh" => "nohlsearch",
        other => other,
    }
}

pub fn parse(input: &str) -> Command {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Command::Empty;
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();
    match head {
        "q" | "quit" | "exit" => Command::Quit,
        "open" | "o" => Command::Open(arg.to_owned()),
        "close" => Command::Close,
        "refresh" | "r" => Command::Refresh,
        "run" => Command::Run,
        "run-all" | "runall" => Command::RunAll,
        "stream" => Command::Stream,
        "stream-all" | "streamall" => Command::StreamAll,
        "cancel" => Command::Cancel,
        "clear" => Command::Clear,
        "explain" => Command::Explain,
        "export" => parse_export(arg),
        "dump-schema" | "dumpschema" => parse_dump(arg),
        "add" => Command::Add,
        "next" | "next-page" | "npage" => Command::NextPage,
        "prev" | "prev-page" | "ppage" => Command::PrevPage,
        "page-size" | "pagesize" => match arg.parse::<usize>() {
            Ok(n) if n > 0 => Command::PageSize(n),
            _ => Command::Unknown("page-size: expected a positive integer".into()),
        },
        "begin" | "start" => {
            if arg.is_empty() {
                Command::Begin(None)
            } else {
                match IsolationArg::parse(arg) {
                    Some(iso) => Command::Begin(Some(iso)),
                    None => Command::Unknown(format!("begin: unknown isolation '{arg}'")),
                }
            }
        }
        "commit" => Command::Commit,
        "rollback" | "abort" => {
            if arg.is_empty() {
                Command::Rollback
            } else {
                Command::RollbackTo(arg.to_owned())
            }
        }
        "savepoint" | "sp" => {
            if arg.is_empty() {
                Command::Unknown("savepoint: name required".into())
            } else {
                Command::Savepoint(arg.to_owned())
            }
        }
        "release" => {
            if arg.is_empty() {
                Command::Unknown("release: savepoint name required".into())
            } else {
                Command::Release(arg.to_owned())
            }
        }
        "rollback-to" | "rollbackto" => {
            if arg.is_empty() {
                Command::Unknown("rollback-to: savepoint name required".into())
            } else {
                Command::RollbackTo(arg.to_owned())
            }
        }
        "remove" | "rm" => {
            if arg.is_empty() {
                Command::Unknown("remove: connection name required".into())
            } else {
                Command::Remove(arg.to_owned())
            }
        }
        "forget" => {
            if arg.is_empty() {
                Command::Unknown("forget: connection name required".into())
            } else {
                Command::Forget(arg.to_owned())
            }
        }
        "plug-load" | "plugload" | "plug" => {
            if arg.is_empty() {
                Command::Unknown("plug-load: path to .lua file required".into())
            } else {
                Command::PluginLoad(arg.to_owned())
            }
        }
        "plug-list" | "pluglist" | "plugins" => Command::PluginList,
        "history" => Command::History,
        "new" | "tabnew" => Command::NewTab,
        "tabclose" | "tc" => Command::CloseTab,
        "tabnext" | "tn" => Command::NextTab,
        "tabprev" | "tp" | "tabprevious" => Command::PrevTab,
        "help" | "h" => {
            if arg.is_empty() {
                Command::Help(None)
            } else {
                Command::Help(Some(arg.to_owned()))
            }
        }
        "nohlsearch" | "noh" => Command::NoHlSearch,
        _ => {
            // Try substitute: s/pat/rep/[gc] or %s/pat/rep/[gc]
            if let Some(cmd) = try_parse_substitute(trimmed) {
                cmd
            } else {
                Command::Unknown(trimmed.to_owned())
            }
        }
    }
}

fn parse_dump(arg: &str) -> Command {
    let trimmed = arg.trim();
    let target = match trimmed {
        "" => DumpTarget::Current,
        "*" | "all" => DumpTarget::All,
        name => DumpTarget::Named(name.to_owned()),
    };
    Command::DumpSchema { target }
}

fn parse_export(arg: &str) -> Command {
    let mut parts = arg.split_whitespace();
    let Some(format) = parts.next() else {
        return Command::Unknown("export: format required (csv|json|insert)".into());
    };
    let Some(path) = parts.next() else {
        return Command::Unknown("export: path required".into());
    };
    if parts.next().is_some() {
        return Command::Unknown("export: too many arguments".into());
    }
    Command::Export {
        format: format.to_owned(),
        path: path.to_owned(),
    }
}

/// Try to parse `:s/pat/rep/[gc]` or `:%s/pat/rep/[gc]`.
/// Returns `None` if the input doesn't match the substitute pattern.
fn try_parse_substitute(input: &str) -> Option<Command> {
    let (range, rest) = if let Some(r) = input.strip_prefix("%s/") {
        (SubstituteRange::WholeBuffer, r)
    } else if let Some(r) = input.strip_prefix("s/") {
        (SubstituteRange::CurrentLine, r)
    } else {
        return None;
    };

    // Split on `/` — we need at least pattern/replacement/
    let mut slash_iter = rest.splitn(3, '/');
    let pattern = slash_iter.next().unwrap_or("").to_owned();
    let replacement = slash_iter.next().unwrap_or("").to_owned();
    let flags = slash_iter.next().unwrap_or("");

    if pattern.is_empty() {
        return Some(Command::Unknown("substitute: empty pattern".into()));
    }

    let global = flags.contains('g');
    let confirm = flags.contains('c');

    Some(Command::Substitute {
        range,
        pattern,
        replacement,
        global,
        confirm,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases() {
        assert_eq!(parse(":q"), Command::Unknown(":q".into()));
        assert_eq!(parse("q"), Command::Quit);
        assert_eq!(parse("quit"), Command::Quit);
        assert_eq!(parse("exit"), Command::Quit);
        assert_eq!(parse("o prod"), Command::Open("prod".into()));
        assert_eq!(parse("open  prod-db  "), Command::Open("prod-db".into()));
        assert_eq!(parse("run-all"), Command::RunAll);
        assert_eq!(parse("stream"), Command::Stream);
        assert_eq!(parse("stream-all"), Command::StreamAll);
        assert_eq!(
            parse("export csv /tmp/out.csv"),
            Command::Export {
                format: "csv".into(),
                path: "/tmp/out.csv".into(),
            }
        );
        assert_eq!(
            parse("dump-schema"),
            Command::DumpSchema {
                target: DumpTarget::Current
            }
        );
        assert_eq!(
            parse("dump-schema all"),
            Command::DumpSchema {
                target: DumpTarget::All
            }
        );
        assert_eq!(
            parse("dump-schema orders"),
            Command::DumpSchema {
                target: DumpTarget::Named("orders".into())
            }
        );
        assert_eq!(parse("new"), Command::NewTab);
        assert_eq!(parse("tabnew"), Command::NewTab);
        assert_eq!(parse("tabclose"), Command::CloseTab);
        assert_eq!(parse("tabnext"), Command::NextTab);
        assert_eq!(parse("tabprev"), Command::PrevTab);
        assert_eq!(parse("next"), Command::NextPage);
        assert_eq!(parse("prev-page"), Command::PrevPage);
        assert_eq!(parse("page-size 50"), Command::PageSize(50));
        match parse("page-size 0") {
            Command::Unknown(msg) => assert!(msg.contains("positive")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        assert_eq!(parse("begin"), Command::Begin(None));
        assert_eq!(
            parse("begin serializable"),
            Command::Begin(Some(IsolationArg::Serializable))
        );
        assert_eq!(
            parse("begin read-committed"),
            Command::Begin(Some(IsolationArg::ReadCommitted))
        );
        assert_eq!(parse("commit"), Command::Commit);
        assert_eq!(parse("rollback"), Command::Rollback);
        assert_eq!(parse("rollback sp1"), Command::RollbackTo("sp1".into()));
        assert_eq!(parse("savepoint sp1"), Command::Savepoint("sp1".into()));
        assert_eq!(parse("sp sp2"), Command::Savepoint("sp2".into()));
        assert_eq!(parse("release sp1"), Command::Release("sp1".into()));
        assert_eq!(parse("rollback-to sp1"), Command::RollbackTo("sp1".into()));
        match parse("begin bogus") {
            Command::Unknown(msg) => assert!(msg.contains("isolation")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        assert_eq!(parse("remove dev"), Command::Remove("dev".into()));
        assert_eq!(parse("rm  prod "), Command::Remove("prod".into()));
        assert_eq!(parse("forget dev"), Command::Forget("dev".into()));
        match parse("remove") {
            Command::Unknown(msg) => assert!(msg.contains("connection name")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        match parse("export") {
            Command::Unknown(msg) => assert!(msg.contains("format required")),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn empty_and_unknown() {
        assert_eq!(parse(""), Command::Empty);
        assert_eq!(parse("   "), Command::Empty);
        assert_eq!(parse("zz"), Command::Unknown("zz".into()));
    }

    /// `:help <cmd>` walks BUILTIN_COMMAND_DESCRIPTIONS after
    /// resolving aliases through `resolve_builtin_alias`. The lookup
    /// only stays useful as long as every parser-accepted built-in
    /// (and every alias it accepts) resolves to a primary key that
    /// has a description entry. Without this test, adding a new
    /// command to BUILTIN_COMMAND_NAMES + parser without touching
    /// BUILTIN_COMMAND_DESCRIPTIONS silently makes `:help <newcmd>`
    /// report "unknown command".
    #[test]
    fn every_builtin_command_name_has_a_description() {
        for &name in BUILTIN_COMMAND_NAMES {
            let primary = resolve_builtin_alias(name);
            assert!(
                BUILTIN_COMMAND_DESCRIPTIONS
                    .iter()
                    .any(|(key, _)| *key == primary),
                "BUILTIN_COMMAND_NAMES contains '{name}' (resolves to '{primary}') \
                 but BUILTIN_COMMAND_DESCRIPTIONS has no entry for it"
            );
        }
    }
}
