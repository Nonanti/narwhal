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
    Help,
    Unknown(String),
    Empty,
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
];

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
        "new" | "tabnew" => Command::NewTab,
        "tabclose" | "tc" => Command::CloseTab,
        "tabnext" | "tn" => Command::NextTab,
        "tabprev" | "tp" | "tabprevious" => Command::PrevTab,
        "help" | "h" => Command::Help,
        _ => Command::Unknown(trimmed.to_owned()),
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
        return Command::Unknown("export: format required (csv|json)".into());
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
}
