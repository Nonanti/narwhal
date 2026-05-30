/// Selector for [`Command::DumpSchema`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DumpTarget {
    /// Dump the table currently shown in the result pane (`TableDetail`).
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
    /// Pretty-print the SQL statement under the cursor in place. Uses
    /// the active session's dialect when one is open, otherwise the
    /// generic profile.
    Format,
    /// Pretty-print every statement in the editor buffer.
    FormatAll,
    /// Pre-fill the connection wizard from a connection URL
    /// (`:url postgres://user:pass@host/db`). The user can still tweak
    /// the form before saving.
    Url(String),
    /// Test connectivity. With no argument, pings the active session;
    /// with an argument, opens a transient session (looking the name up
    /// in `connections.toml` or parsing the argument as a URL) and
    /// closes it immediately.
    Test(Option<String>),
    /// Open the connection wizard pre-filled from an existing saved
    /// connection (`:edit <name>`). Committing the wizard updates the
    /// entry in place and rewrites its keyring secret.
    Edit(String),
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
    /// Open the Ctrl+R history modal. With `Some(pattern)` pre-fills
    /// the filter (v1.3 #11).
    History(Option<String>),
    /// L36 #1: open the staged-mutation preview modal. Discoverable
    /// counterpart to the `Ctrl-P` chord for users who live in the
    /// command line.
    Pending,
    /// v1.2 #5: flush every pending mutation inside one transaction.
    /// Equivalent to `Ctrl-S` while the pending preview is open.
    Submit,
    /// v1.2 #5: throw away every pending mutation without writing.
    /// Equivalent to `Ctrl-X` inside the preview.
    Revert,
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
    /// Save the current editor buffer as a named snippet (`:save <name>`).
    SaveSnippet {
        name: String,
    },
    /// Load a named snippet into a new editor tab (`:load <name>`).
    LoadSnippet {
        name: String,
    },
    /// Delete a named snippet (`:rm-snippet <name>`).
    RemoveSnippet {
        name: String,
    },
    /// Open the snippets modal (`:snippets`).
    ListSnippets,
    /// v1.1 #1: open the fuzzy schema navigator (`:goto` / `:g` / Ctrl-N).
    /// Lists every table/view across all loaded schemas, picks one
    /// via fuzzy match, inserts `<schema>.<table>` at the cursor on
    /// confirm.
    Goto,
    /// v1.2 #7: set the result-pane filter (`:filter <expr>`) or
    /// clear it (`:filter clear`). Same expression that the inline
    /// `f` prompt accepts; commits immediately.
    Filter(Option<String>),
    /// v1.2 #7: toggle a sort on a 1-based column number
    /// (`:sort 3`) or clear the active sort (`:sort clear`).
    Sort(SortArg),
    /// v1.2 #8: emit ALTER TABLE migration SQL between two tables in
    /// the active connection. Format: `:diff-schema a.tbl1 b.tbl2`.
    /// The result lands in a fresh editor tab so the user can review
    /// before executing.
    DiffSchema { left: String, right: String },
    /// v1.3 #9: run the lint rule set over the active buffer and
    /// dump findings to a fresh tab. `:lint` reuses the active tab's
    /// content; no argument needed.
    Lint,
    /// v1.3 #10: insert a built-in SQL template at the cursor.
    /// `:tpl sel` etc.; `:tpl` (no arg) shows the available names.
    Template(Option<String>),
    Unknown(String),
    Empty,
}

/// Argument to [`Command::Sort`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortArg {
    /// 1-based column number. Toggle ascending → descending → cleared.
    Column(usize),
    /// `:sort clear` — drop the active sort.
    Clear,
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
///
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
    "format",
    "fmt",
    "format-all",
    "fmtall",
    "url",
    "test",
    "edit",
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
    "save",
    "load",
    "rm-snippet",
    "rmsnippet",
    "snippets",
];

/// Short descriptions for built-in commands, looked up by `:help <name>`.
///
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
        "format",
        "pretty-print the SQL under the cursor (also :fmt)",
    ),
    (
        "format-all",
        "pretty-print every statement in the buffer (also :fmtall)",
    ),
    (
        "url",
        "open the wizard pre-filled from a DSN (:url postgres://user:pw@host/db)",
    ),
    (
        "test",
        "test connectivity (:test [name|url]); no arg pings the active session",
    ),
    (
        "edit",
        "edit a saved connection in the wizard (:edit <name>)",
    ),
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
    (
        "save",
        "save the current editor buffer as a named snippet (:save <name>)",
    ),
    (
        "load",
        "load a named snippet into a new editor tab (:load <name>)",
    ),
    ("rm-snippet", "delete a named snippet (:rm-snippet <name>)"),
    (
        "snippets",
        "open the snippets modal to browse and load saved queries",
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
        "fmt" => "format",
        "fmtall" => "format-all",
        "plugload" | "plug" => "plug-load",
        "pluglist" | "plugins" => "plug-list",
        "history" => "history",
        "tabnew" => "new",
        "tc" => "tabclose",
        "tn" => "tabnext",
        "tp" | "tabprevious" => "tabprev",
        "h" => "help",
        "noh" => "nohlsearch",
        "rmsnippet" => "rm-snippet",
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
        "format" | "fmt" => Command::Format,
        "format-all" | "fmtall" => Command::FormatAll,
        "url" => {
            if arg.is_empty() {
                Command::Unknown("url: dsn required (e.g. :url postgres://user@host/db)".into())
            } else {
                Command::Url(arg.to_owned())
            }
        }
        "test" => {
            if arg.is_empty() {
                Command::Test(None)
            } else {
                Command::Test(Some(arg.to_owned()))
            }
        }
        "edit" => {
            if arg.is_empty() {
                Command::Unknown("edit: connection name required".into())
            } else {
                Command::Edit(arg.to_owned())
            }
        }
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
        "history" => {
            let p = arg.trim();
            if p.is_empty() {
                Command::History(None)
            } else {
                // Allow `:history /pattern` (drop the leading slash)
                // for symmetry with vim's reverse search.
                let stripped = p.strip_prefix('/').unwrap_or(p);
                Command::History(Some(stripped.to_owned()))
            }
        }
        "pending" => Command::Pending,
        // v1.2 #8: `:diff` *without* args still opens the pending
        // preview (legacy alias). With args it's a schema diff.
        "diff" => {
            let trimmed = arg.trim();
            if trimmed.is_empty() {
                Command::Pending
            } else {
                let mut parts = trimmed.split_whitespace();
                match (parts.next(), parts.next()) {
                    (Some(l), Some(r)) => Command::DiffSchema {
                        left: l.to_owned(),
                        right: r.to_owned(),
                    },
                    _ => Command::Unknown(
                        "diff: expected two qualified table names, e.g. :diff public.a public.b"
                            .into(),
                    ),
                }
            }
        }
        "lint" => Command::Lint,
        "tpl" | "template" => {
            let trimmed = arg.trim();
            if trimmed.is_empty() {
                Command::Template(None)
            } else {
                Command::Template(Some(trimmed.to_owned()))
            }
        }
        "diff-schema" => {
            let mut parts = arg.split_whitespace();
            match (parts.next(), parts.next()) {
                (Some(l), Some(r)) => Command::DiffSchema {
                    left: l.to_owned(),
                    right: r.to_owned(),
                },
                _ => Command::Unknown(
                    "diff-schema: expected two qualified table names".into(),
                ),
            }
        }
        "submit" | "commit-pending" => Command::Submit,
        "revert" | "discard-pending" => Command::Revert,
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
        "save" => {
            if arg.is_empty() {
                Command::Unknown("save: snippet name required".into())
            } else {
                Command::SaveSnippet {
                    name: arg.to_owned(),
                }
            }
        }
        "load" => {
            if arg.is_empty() {
                Command::Unknown("load: snippet name required".into())
            } else {
                Command::LoadSnippet {
                    name: arg.to_owned(),
                }
            }
        }
        "rm-snippet" | "rmsnippet" => {
            if arg.is_empty() {
                Command::Unknown("rm-snippet: snippet name required".into())
            } else {
                Command::RemoveSnippet {
                    name: arg.to_owned(),
                }
            }
        }
        "snippets" => Command::ListSnippets,
        "goto" | "g" => Command::Goto,
        "filter" => {
            // `:filter` (no arg)        → open inline prompt
            // `:filter clear`           → drop the active filter
            // `:filter <expr>`          → set the filter expression
            if arg.trim().is_empty() {
                Command::Filter(None)
            } else if arg.trim().eq_ignore_ascii_case("clear") {
                Command::Filter(Some(String::new()))
            } else {
                Command::Filter(Some(arg.to_owned()))
            }
        }
        "sort" => {
            let arg = arg.trim();
            if arg.is_empty() || arg.eq_ignore_ascii_case("clear") {
                Command::Sort(SortArg::Clear)
            } else {
                match arg.parse::<usize>() {
                    Ok(n) if n >= 1 => Command::Sort(SortArg::Column(n)),
                    _ => Command::Unknown(
                        "sort: expected a 1-based column number or 'clear'".into(),
                    ),
                }
            }
        }
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
    // Split into `format` + remainder so paths containing spaces stay
    // intact. The split_whitespace + take(2) shape that used to live
    // here rejected `:export csv /tmp/my data.csv` with a confusing
    // "too many arguments" error.
    let trimmed = arg.trim_start();
    let (format, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((f, r)) => (f, r.trim_start()),
        None => (trimmed, ""),
    };
    if format.is_empty() {
        return Command::Unknown("export: format required (csv|json|insert)".into());
    }
    let path = rest.trim_end();
    if path.is_empty() {
        return Command::Unknown("export: path required".into());
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
        // Round 2 bugfix: paths with spaces used to be rejected with
        // a confusing "too many arguments" error.
        assert_eq!(
            parse("export csv /tmp/my data.csv"),
            Command::Export {
                format: "csv".into(),
                path: "/tmp/my data.csv".into(),
            }
        );
        // Trailing whitespace gets trimmed but interior spaces survive.
        assert_eq!(
            parse("export json   /tmp/two words.json   "),
            Command::Export {
                format: "json".into(),
                path: "/tmp/two words.json".into(),
            }
        );
    }

    #[test]
    fn empty_and_unknown() {
        assert_eq!(parse(""), Command::Empty);
        assert_eq!(parse("   "), Command::Empty);
        assert_eq!(parse("zz"), Command::Unknown("zz".into()));
    }

    /// `:help <cmd>` walks `BUILTIN_COMMAND_DESCRIPTIONS` after
    /// resolving aliases through `resolve_builtin_alias`. The lookup
    /// only stays useful as long as every parser-accepted built-in
    /// (and every alias it accepts) resolves to a primary key that
    /// has a description entry. Without this test, adding a new
    /// command to `BUILTIN_COMMAND_NAMES` + parser without touching
    /// `BUILTIN_COMMAND_DESCRIPTIONS` silently makes `:help <newcmd>`
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
