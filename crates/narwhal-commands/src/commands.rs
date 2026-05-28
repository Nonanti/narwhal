/// Parsed flag bag for [`Command::Export`].
///
/// `compression` is `Some` only when the user explicitly passed
/// `--compression <codec>`; the dispatch layer falls back to the
/// format's default codec when it sees `None`. `no_truncate` is the
/// boolean form of `--no-truncate`.
///
/// We keep this as a small typed struct (rather than `Vec<String>`)
/// because the parser already validates the codec token, and the
/// dispatch layer would otherwise have to reparse the strings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportArgs {
    pub compression: Option<crate::export::ParquetCompression>,
    pub no_truncate: bool,
}

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

/// Output format accepted by [`Command::DiagramExport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramFormat {
    /// Mermaid `erDiagram` source (paste into mermaid.live).
    Mermaid,
    /// Graphviz `dot` source (`dot -Tsvg` to render).
    Dot,
}

impl DiagramFormat {
    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "mermaid" | "mmd" | "mer" => Some(Self::Mermaid),
            "dot" | "gv" | "graphviz" => Some(Self::Dot),
            _ => None,
        }
    }

    /// File extension used when the user omits one in `:diagram export`.
    pub const fn default_extension(self) -> &'static str {
        match self {
            Self::Mermaid => "mmd",
            Self::Dot => "dot",
        }
    }

    /// Human-readable label for status messages.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mermaid => "mermaid",
            Self::Dot => "dot",
        }
    }
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
        /// parsed options for the chosen format (parquet
        /// `--compression`, markdown `--no-truncate`, etc). Empty
        /// for csv/json/tsv/table/insert, populated for parquet/md.
        options: ExportArgs,
    },
    DumpSchema {
        target: DumpTarget,
    },
    /// Export an ER diagram of the active connection's schema as
    /// Mermaid (`erDiagram`) or Graphviz (`dot`). When `path` is `None`
    /// the rendered string is copied to the system clipboard; otherwise
    /// it is written to disk.
    ///
    /// `table` restricts the diagram to that table and its 1-hop FK
    /// neighbours; `schema` restricts to a single schema.
    DiagramExport {
        format: DiagramFormat,
        path: Option<String>,
        table: Option<String>,
        schema: Option<String>,
    },
    /// Open the in-TUI diagram modal in *Focused* mode, centred on the
    /// given table. The table token may be `name` or `schema.name`.
    DiagramFocus(String),
    /// Open the in-TUI diagram modal in *Impact* mode (reverse-FK
    /// closure) for the given table.
    DiagramImpact(String),
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
    /// the filter.
    History(Option<String>),
    /// L36 #1: open the staged-mutation preview modal. Discoverable
    /// counterpart to the `Ctrl-P` chord for users who live in the
    /// command line.
    Pending,
    /// flush every pending mutation inside one transaction.
    /// Equivalent to `Ctrl-S` while the pending preview is open.
    Submit,
    /// throw away every pending mutation without writing.
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
    /// open the fuzzy schema navigator (`:goto` / `:g` / Ctrl-N).
    /// Lists every table/view across all loaded schemas, picks one
    /// via fuzzy match, inserts `<schema>.<table>` at the cursor on
    /// confirm.
    Goto,
    /// In-app settings editor (`:settings` / `:set`). Opens the
    /// modal that drives editor mode, mouse mode, theme,
    /// keybinding preset, etc. Commits to `settings.toml` on save.
    Settings,
    /// Quick editor-mode switch (`:mode vim|basic|emacs`). Bypasses
    /// the settings modal; persists to disk via the same code path.
    Mode(String),
    /// set the result-pane filter (`:filter <expr>`) or
    /// clear it (`:filter clear`). Same expression that the inline
    /// `f` prompt accepts; commits immediately.
    Filter(Option<String>),
    /// toggle a sort on a 1-based column number
    /// (`:sort 3`) or clear the active sort (`:sort clear`).
    Sort(SortArg),
    /// emit ALTER TABLE migration SQL between two tables in
    /// the active connection. Format: `:diff-schema a.tbl1 b.tbl2`.
    /// The result lands in a fresh editor tab so the user can review
    /// before executing.
    DiffSchema {
        left: String,
        right: String,
    },
    /// full-schema diff between two **connections**
    /// (`:schema-diff source target`). Opens both transiently,
    /// introspects every user table, runs `narwhal-schema-diff`,
    /// dumps the emitted DDL into a fresh editor tab. Optional
    /// flags pick the dialect, narrow the scope, or rewrite target
    /// schema names. Note this is distinct from `:diff-schema`,
    /// which is single-connection two-table.
    SchemaDiff {
        /// Source connection (desired state).
        source: String,
        /// Target connection (will be migrated).
        target: String,
        /// Override the auto-picked dialect; `None` uses the
        /// source connection's driver name.
        dialect: Option<String>,
        /// Restrict the diff to one schema (both sides).
        schema: Option<String>,
        /// Restrict the diff to one table.
        table: Option<String>,
        /// `(source, target)` schema renames applied to the
        /// target-side introspection before diffing.
        schema_map: Vec<(String, String)>,
    },
    /// run the lint rule set over the active buffer and
    /// dump findings to a fresh tab. `:lint` reuses the active tab's
    /// content; no argument needed.
    Lint,
    /// toggle the inline ASCII chart pane over the active
    /// result. `:chart bar|line|sparkline` activates the pane;
    /// `:chart off` (or `:chart none`) removes it. Optional flags:
    /// `--x col`, `--y col`, `--col col` (sparkline alias), `--title T`.
    Chart(ChartArg),
    /// toggle the inline pivot-table pane over the active
    /// result. `:pivot rows=col[,col..] [cols=col] [value=col]
    /// [agg=count|sum|avg|min|max]` activates; `:pivot off` removes it.
    Pivot(PivotArg),
    /// insert a built-in SQL template at the cursor.
    /// `:tpl sel` etc.; `:tpl` (no arg) shows the available names.
    Template(Option<String>),
    Unknown(String),
    Empty,
}

/// Argument to [`Command::Chart`]. The parser turns the command-line
/// flags into a structured payload so the dispatch layer can wire the
/// chart config in one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChartArg {
    /// Activate the chart pane with the given kind and overrides.
    On {
        /// Token the user typed: `bar`, `line`, or `sparkline`.
        kind: ChartKindArg,
        /// `--title T` override; rendered in the chart's title bar.
        title: Option<String>,
        /// `--x col` override; ignored for sparkline.
        x_col: Option<String>,
        /// `--y col` (or `--col col` for sparkline) override.
        y_col: Option<String>,
    },
    /// `:chart off` / `:chart none` — dismiss the chart pane.
    Off,
}

/// Chart kind token parsed by [`Command::Chart`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartKindArg {
    Bar,
    Line,
    Sparkline,
}

impl ChartKindArg {
    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "bar" => Some(Self::Bar),
            "line" => Some(Self::Line),
            "sparkline" | "spark" => Some(Self::Sparkline),
            _ => None,
        }
    }
}

/// Argument to [`Command::Pivot`]. The parser turns the
/// `key=value` tokens into a structured payload so the dispatch
/// layer can wire the pivot config in one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PivotArg {
    /// Activate the pivot pane with the given config.
    On {
        /// Row-dimension columns (comma-separated on the command line).
        rows: Vec<String>,
        /// Optional column-dimension; when `None` the pivot is a
        /// single collapsed column.
        cols: Option<String>,
        /// Optional value column; required for sum / avg / min / max.
        value: Option<String>,
        /// Aggregator kind token; the dispatch layer maps to
        /// `narwhal_pivot::AggKind`.
        agg: PivotAggArg,
    },
    /// `:pivot off` — dismiss the pivot pane.
    Off,
}

/// Aggregator token parsed by [`Command::Pivot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PivotAggArg {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

impl PivotAggArg {
    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "count" | "cnt" | "n" => Some(Self::Count),
            "sum" | "total" => Some(Self::Sum),
            "avg" | "mean" | "average" => Some(Self::Avg),
            "min" | "minimum" => Some(Self::Min),
            "max" | "maximum" => Some(Self::Max),
            _ => None,
        }
    }
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
    "diagram",
    "diag",
    "schema-diff",
    "schemadiff",
    "chart",
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
    "pivot",
    "settings",
    "set",
    "mode",
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
        "export the current result to a file (:export csv|json|tsv|insert|parquet|markdown <path>)",
    ),
    (
        "dump-schema",
        "write CREATE TABLE DDL into the editor (:dump-schema [name|all])",
    ),
    (
        "diagram",
        "export an ER diagram (:diagram export mermaid|dot [path] [--table T] [--schema S])",
    ),
    (
        "schema-diff",
        "full-schema diff between two connections (:schema-diff src tgt [--dialect ..] [--schema ..] [--table ..] [--schema-map src=tgt])",
    ),
    (
        "chart",
        "render an inline ASCII chart over the active result (:chart bar|line|sparkline [--x col] [--y col] [--title T]; :chart off)",
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
    (
        "pivot",
        "render an inline pivot table over the active result (:pivot rows=col[,col..] [cols=col] [value=col] [agg=count|sum|avg|min|max]; :pivot off)",
    ),
    (
        "settings",
        "open the in-app settings editor (:settings / :set) — editor mode, mouse, theme, keybindings",
    ),
    (
        "mode",
        "switch the editor input model on the fly (:mode vim|basic|emacs); persists to settings.toml",
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
        "diag" => "diagram",
        "schemadiff" => "schema-diff",
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
        "set" => "settings",
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
        "diagram" | "diag" => parse_diagram(arg),
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
        // `:diff` *without* args still opens the pending
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
        "pivot" => parse_pivot(arg),
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
                _ => Command::Unknown("diff-schema: expected two qualified table names".into()),
            }
        }
        "schema-diff" | "schemadiff" => parse_schema_diff(arg),
        "chart" => parse_chart(arg),
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
        "settings" | "set" => Command::Settings,
        "mode" => Command::Mode(arg.to_owned()),
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
                    _ => {
                        Command::Unknown("sort: expected a 1-based column number or 'clear'".into())
                    }
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
        return Command::Unknown(
            "export: format required (csv|json|tsv|table|insert|parquet|markdown)".into(),
        );
    }
    // the path may be followed by `--compression <codec>` or
    // `--no-truncate`. Strip the trailing flags first so a path with
    // spaces survives; what remains is the path proper. The flags are
    // intentionally trailing-only so `:export markdown out.md` keeps
    // the historical positional shape.
    let (path, options) = match split_export_flags(rest) {
        Ok(parsed) => parsed,
        Err(msg) => return Command::Unknown(msg),
    };
    let path = path.trim_end();
    if path.is_empty() {
        return Command::Unknown("export: path required".into());
    }
    Command::Export {
        format: format.to_owned(),
        path: path.to_owned(),
        options,
    }
}

/// Trailing-flag parser for `:export`. Walks tokens from the right
/// peeling off `--no-truncate` and `--compression <codec>` until a
/// non-flag token is hit; everything before that point is the path.
///
/// The trailing-only contract is what lets the path keep interior
/// spaces (`:export markdown /tmp/my report.md --no-truncate`): once
/// we stop seeing flags, the entire prefix — spaces and all — is the
/// path.
fn split_export_flags(rest: &str) -> Result<(&str, ExportArgs), String> {
    let mut args = ExportArgs::default();
    let mut cursor = rest.trim_end();
    loop {
        // Peel the last whitespace-separated token off `cursor`.
        let (head, last) = match cursor.rfind(char::is_whitespace) {
            Some(boundary) => {
                let (prefix, suffix) = cursor.split_at(boundary);
                (prefix.trim_end(), suffix.trim_start())
            }
            None => ("", cursor),
        };
        match last {
            "--no-truncate" => {
                args.no_truncate = true;
                cursor = head;
            }
            "--compression" => {
                // Bare `--compression` with no value is a user error.
                return Err("export: --compression requires a codec (snappy|zstd|none)".into());
            }
            // Any other token might be the path or the value half of
            // `--compression <codec>`. Peek one further left.
            value if !value.is_empty() => {
                let (head2, prev) = match head.rfind(char::is_whitespace) {
                    Some(b) => {
                        let (p, s) = head.split_at(b);
                        (p.trim_end(), s.trim_start())
                    }
                    None => ("", head),
                };
                if prev == "--compression" {
                    let Some(codec) = crate::export::ParquetCompression::from_token(value) else {
                        return Err(format!(
                            "export: unknown compression `{value}` (snappy|zstd|none)"
                        ));
                    };
                    args.compression = Some(codec);
                    cursor = head2;
                    continue;
                }
                // Not a flag pair — stop; what we have is the path.
                break;
            }
            _ => break,
        }
    }
    Ok((cursor, args))
}

/// Parse the argument to `:diagram`. Grammar:
///
/// ```text
/// export <format> [path] [--table NAME] [--schema NAME]
/// impact <table>
/// focus  <table>                # explicit Focused modal
/// -- <table>                    # bare-table escape (when table is `export` / `impact` / `focus`)
/// <table>                       # implicit Focused modal (muscle-memory form)
/// ```
///
/// `format` is one of `mermaid|mmd|mer|dot|gv|graphviz`. `path` is the
/// first positional argument that is not a flag; if absent, the rendered
/// diagram goes to the system clipboard. `--table` / `-t` restricts the
/// diagram to a single table and its 1-hop FK neighbours.
///
/// The bare `<table>` form is intentionally positional so muscle-memory
/// like `:diagram users` works without remembering a subcommand. If
/// the table is literally named `export`, `impact`, or `focus` (rare
/// but legal), use `:diagram -- export` to force the focused-modal path
/// or `:diagram focus export` for the same effect spelled out.
fn parse_diagram(arg: &str) -> Command {
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        return Command::Unknown(
            "diagram: subcommand or table name required (try `:diagram users`)".into(),
        );
    }
    let (sub, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((s, r)) => (s, r.trim()),
        None => (trimmed, ""),
    };
    match sub {
        "export" => parse_diagram_export(rest),
        "impact" => parse_diagram_table_subcommand("impact", rest, Command::DiagramImpact),
        "focus" => parse_diagram_table_subcommand("focus", rest, Command::DiagramFocus),
        // `:diagram -- <table>` escapes a literal table named
        // `export`, `impact` or `focus`. The escape must be followed
        // by exactly one argument; otherwise the user almost certainly
        // meant something else.
        "--" => {
            if rest.is_empty() {
                return Command::Unknown("diagram --: table name required after the escape".into());
            }
            if rest.split_whitespace().count() > 1 {
                return Command::Unknown(format!(
                    "diagram --: unexpected extra arguments after '{}'",
                    rest.split_whitespace().next().unwrap_or("")
                ));
            }
            Command::DiagramFocus(rest.to_owned())
        }
        // Bare positional: sub *is* the table name.
        table => {
            if !rest.is_empty() {
                return Command::Unknown(format!(
                    "diagram: unexpected argument '{rest}' after table name"
                ));
            }
            Command::DiagramFocus(table.to_owned())
        }
    }
}

/// Shared body for `:diagram impact <table>` and `:diagram focus <table>`.
/// Both subcommands take exactly one positional argument; reject every
/// other shape with a friendly error.
fn parse_diagram_table_subcommand(
    sub: &str,
    rest: &str,
    builder: fn(String) -> Command,
) -> Command {
    if rest.is_empty() {
        return Command::Unknown(format!("diagram {sub}: table name required"));
    }
    if rest.split_whitespace().count() > 1 {
        return Command::Unknown(format!(
            "diagram {sub}: unexpected extra arguments after '{}'",
            rest.split_whitespace().next().unwrap_or("")
        ));
    }
    builder(rest.to_owned())
}

/// Parse `:schema-diff source target [--dialect d] [--schema s]
/// [--table t] [--schema-map src=tgt]...`.
///
/// Two positional args (source + target connection names) are
/// required. Repeating `--schema-map` builds the rename map. The
/// `--out` flag is intentionally **omitted** from the TUI parser:
/// the in-editor render is the whole point of the TUI command. Use
/// the `narwhal schema-diff` headless subcommand when you want a
/// file.
fn parse_schema_diff(arg: &str) -> Command {
    let mut tokens = arg.split_whitespace();
    let mut positional: Vec<String> = Vec::new();
    let mut dialect: Option<String> = None;
    let mut schema: Option<String> = None;
    let mut table: Option<String> = None;
    let mut schema_map: Vec<(String, String)> = Vec::new();

    while let Some(tok) = tokens.next() {
        match tok {
            "--dialect" => {
                let Some(value) = tokens.next() else {
                    return Command::Unknown("schema-diff: --dialect requires a value".into());
                };
                dialect = Some(value.to_owned());
            }
            "--schema" => {
                let Some(value) = tokens.next() else {
                    return Command::Unknown("schema-diff: --schema requires a value".into());
                };
                schema = Some(value.to_owned());
            }
            "--table" => {
                let Some(value) = tokens.next() else {
                    return Command::Unknown("schema-diff: --table requires a value".into());
                };
                table = Some(value.to_owned());
            }
            "--schema-map" => {
                let Some(value) = tokens.next() else {
                    return Command::Unknown(
                        "schema-diff: --schema-map requires a source=target value".into(),
                    );
                };
                let Some((src, tgt)) = value.split_once('=') else {
                    return Command::Unknown(format!(
                        "schema-diff: --schema-map expects `source=target`, got `{value}`"
                    ));
                };
                if src.is_empty() || tgt.is_empty() {
                    return Command::Unknown(format!(
                        "schema-diff: --schema-map: empty side in `{value}`"
                    ));
                }
                schema_map.push((src.to_owned(), tgt.to_owned()));
            }
            other if other.starts_with("--") => {
                return Command::Unknown(format!("schema-diff: unknown flag '{other}'"));
            }
            other => positional.push(other.to_owned()),
        }
    }

    if positional.len() != 2 {
        return Command::Unknown(
            "schema-diff: expected two connection names \
             (`:schema-diff source target [--dialect ..] [--schema ..] [--table ..] \
             [--schema-map src=tgt]`)"
                .into(),
        );
    }
    let target = positional.pop().expect("len == 2");
    let source = positional.pop().expect("len == 2");

    Command::SchemaDiff {
        source,
        target,
        dialect,
        schema,
        table,
        schema_map,
    }
}

/// Parse the argument to `:chart`. Grammar:
///
/// ```text
/// bar|line|sparkline [--x col] [--y col] [--col col] [--title T]
/// off|none
/// ```
///
/// `--col` is an alias for `--y` so the sparkline form
/// (`:chart sparkline --col revenue`) reads naturally. `--x` on a
/// sparkline is silently accepted but stored — the renderer ignores
/// it.
fn parse_chart(arg: &str) -> Command {
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        return Command::Unknown("chart: subcommand required (bar|line|sparkline|off)".into());
    }
    let mut tokens = trimmed.split_whitespace();
    let Some(head) = tokens.next() else {
        return Command::Unknown("chart: subcommand required (bar|line|sparkline|off)".into());
    };
    let lower = head.to_ascii_lowercase();
    if matches!(lower.as_str(), "off" | "none" | "hide") {
        if tokens.next().is_some() {
            return Command::Unknown("chart off: no extra arguments expected".into());
        }
        return Command::Chart(ChartArg::Off);
    }
    let Some(kind) = ChartKindArg::from_token(&lower) else {
        return Command::Unknown(format!(
            "chart: unknown subcommand '{head}' (expected bar|line|sparkline|off)"
        ));
    };

    let mut x_col: Option<String> = None;
    let mut y_col: Option<String> = None;
    let mut title: Option<String> = None;

    while let Some(tok) = tokens.next() {
        match tok {
            "--x" | "-x" => {
                let Some(value) = tokens.next() else {
                    return Command::Unknown("chart: --x requires a column name".into());
                };
                x_col = Some(value.to_owned());
            }
            "--y" | "-y" | "--col" | "-c" => {
                let Some(value) = tokens.next() else {
                    return Command::Unknown("chart: --y / --col requires a column name".into());
                };
                y_col = Some(value.to_owned());
            }
            "--title" | "-t" => {
                let Some(value) = tokens.next() else {
                    return Command::Unknown("chart: --title requires a value".into());
                };
                title = Some(value.to_owned());
            }
            other if other.starts_with("--") => {
                return Command::Unknown(format!("chart: unknown flag '{other}'"));
            }
            other => {
                return Command::Unknown(format!(
                    "chart: unexpected argument '{other}' (use --x / --y / --title)"
                ));
            }
        }
    }

    Command::Chart(ChartArg::On {
        kind,
        title,
        x_col,
        y_col,
    })
}

fn parse_diagram_export(rest: &str) -> Command {
    let mut tokens = rest.split_whitespace();
    let Some(format_tok) = tokens.next() else {
        return Command::Unknown("diagram: format required (mermaid|dot)".into());
    };
    let Some(format) = DiagramFormat::from_token(format_tok) else {
        return Command::Unknown(format!(
            "diagram: unknown format '{format_tok}' (mermaid|dot)"
        ));
    };

    let mut path: Option<String> = None;
    let mut table: Option<String> = None;
    let mut schema: Option<String> = None;
    while let Some(tok) = tokens.next() {
        match tok {
            "--table" | "-t" => {
                let Some(value) = tokens.next() else {
                    return Command::Unknown("diagram: --table requires a table name".into());
                };
                table = Some(value.to_owned());
            }
            "--schema" | "-s" => {
                let Some(value) = tokens.next() else {
                    return Command::Unknown("diagram: --schema requires a schema name".into());
                };
                schema = Some(value.to_owned());
            }
            other if other.starts_with("--") => {
                return Command::Unknown(format!("diagram: unknown flag '{other}'"));
            }
            other => {
                if path.is_some() {
                    return Command::Unknown(format!("diagram: unexpected argument '{other}'"));
                }
                path = Some(other.to_owned());
            }
        }
    }

    Command::DiagramExport {
        format,
        path,
        table,
        schema,
    }
}

/// Try to parse `:s/pat/rep/[gc]` or `:%s/pat/rep/[gc]`.
/// Returns `None` if the input doesn't match the substitute pattern.
/// Parse the argument to `:pivot`. Grammar:
///
/// ```text
/// rows=col1[,col2..] [cols=col] [value=col] [agg=count|sum|avg|min|max]
/// off|none
/// ```
///
/// Tokens are `key=value` pairs; order-independent. `rows=` accepts a
/// comma-separated list and may be omitted only when `cols=` is
/// present. Missing `agg` defaults to `count`.
fn parse_pivot(arg: &str) -> Command {
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        return Command::Unknown(
            "pivot: subcommand required (rows=col[,col..] [cols=col] [value=col] [agg=fn]; or off)"
                .into(),
        );
    }
    let lower_head = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(lower_head.as_str(), "off" | "none" | "hide") {
        if trimmed.split_whitespace().count() > 1 {
            return Command::Unknown("pivot off: no extra arguments expected".into());
        }
        return Command::Pivot(PivotArg::Off);
    }

    let mut rows: Vec<String> = Vec::new();
    let mut cols: Option<String> = None;
    let mut value: Option<String> = None;
    let mut agg: PivotAggArg = PivotAggArg::Count;
    let mut saw_agg = false;

    for token in trimmed.split_whitespace() {
        let Some((key, val)) = token.split_once('=') else {
            return Command::Unknown(format!("pivot: expected key=value pair, got '{token}'"));
        };
        if val.is_empty() {
            return Command::Unknown(format!("pivot: '{key}' needs a value"));
        }
        match key.to_ascii_lowercase().as_str() {
            "rows" | "row" | "r" => {
                rows = val
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
                if rows.is_empty() {
                    return Command::Unknown("pivot: rows= needs at least one column".into());
                }
            }
            "cols" | "col" | "c" => cols = Some(val.to_owned()),
            "value" | "val" | "v" => value = Some(val.to_owned()),
            "agg" | "a" => {
                let Some(parsed) = PivotAggArg::from_token(val) else {
                    return Command::Unknown(format!(
                        "pivot: unknown aggregator '{val}' (expected count|sum|avg|min|max)"
                    ));
                };
                agg = parsed;
                saw_agg = true;
            }
            other => {
                return Command::Unknown(format!("pivot: unknown key '{other}'"));
            }
        }
    }

    if rows.is_empty() && cols.is_none() {
        return Command::Unknown("pivot: at least one of rows= or cols= is required".into());
    }
    // sum / avg / min / max all need a value column. The dispatch
    // layer will surface a richer error if the column is non-numeric.
    if matches!(
        agg,
        PivotAggArg::Sum | PivotAggArg::Avg | PivotAggArg::Min | PivotAggArg::Max
    ) && value.is_none()
    {
        return Command::Unknown(format!(
            "pivot: agg={} requires value=<col>",
            match agg {
                PivotAggArg::Sum => "sum",
                PivotAggArg::Avg => "avg",
                PivotAggArg::Min => "min",
                PivotAggArg::Max => "max",
                PivotAggArg::Count => unreachable!(),
            }
        ));
    }
    let _ = saw_agg; // suppress unused-var lint without `_` rename

    Command::Pivot(PivotArg::On {
        rows,
        cols,
        value,
        agg,
    })
}

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
                options: ExportArgs::default(),
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
        assert_eq!(
            parse("diagram export mermaid"),
            Command::DiagramExport {
                format: DiagramFormat::Mermaid,
                path: None,
                table: None,
                schema: None,
            }
        );
        assert_eq!(
            parse("diag export dot ./schema.dot"),
            Command::DiagramExport {
                format: DiagramFormat::Dot,
                path: Some("./schema.dot".into()),
                table: None,
                schema: None,
            }
        );
        assert_eq!(
            parse("diagram export mmd --table users"),
            Command::DiagramExport {
                format: DiagramFormat::Mermaid,
                path: None,
                table: Some("users".into()),
                schema: None,
            }
        );
        assert_eq!(
            parse("diagram export mermaid /tmp/x.mmd -t orders -s public"),
            Command::DiagramExport {
                format: DiagramFormat::Mermaid,
                path: Some("/tmp/x.mmd".into()),
                table: Some("orders".into()),
                schema: Some("public".into()),
            }
        );
        match parse("diagram export") {
            Command::Unknown(msg) => assert!(msg.contains("format")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        match parse("diagram export svg") {
            Command::Unknown(msg) => assert!(msg.contains("unknown format")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        // Bare positional `:diagram <table>` opens Focused modal.
        assert_eq!(
            parse("diagram users"),
            Command::DiagramFocus("users".into())
        );
        assert_eq!(
            parse("diagram public.orders"),
            Command::DiagramFocus("public.orders".into())
        );
        assert_eq!(parse("diag orders"), Command::DiagramFocus("orders".into()));
        // `:diagram impact <table>` opens Impact modal.
        assert_eq!(
            parse("diagram impact users"),
            Command::DiagramImpact("users".into())
        );
        match parse("diagram impact") {
            Command::Unknown(msg) => assert!(msg.contains("table name required")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        match parse("diagram impact users extra") {
            Command::Unknown(msg) => assert!(msg.contains("unexpected extra")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        match parse("diagram users extra") {
            Command::Unknown(msg) => assert!(msg.contains("unexpected argument")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        // `:diagram focus <table>` spells out the implicit form so
        // tables literally named `export` / `impact` / `focus` are
        // reachable.
        assert_eq!(
            parse("diagram focus export"),
            Command::DiagramFocus("export".into())
        );
        assert_eq!(
            parse("diagram focus public.impact"),
            Command::DiagramFocus("public.impact".into())
        );
        match parse("diagram focus") {
            Command::Unknown(msg) => assert!(msg.contains("table name required")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        // `:diagram -- <table>` escape for the muscle-memory form when
        // the table literally collides with a subcommand name.
        assert_eq!(
            parse("diagram -- export"),
            Command::DiagramFocus("export".into())
        );
        assert_eq!(
            parse("diagram -- impact"),
            Command::DiagramFocus("impact".into())
        );
        match parse("diagram --") {
            Command::Unknown(msg) => assert!(msg.contains("table name required")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        match parse("diagram -- a b") {
            Command::Unknown(msg) => assert!(msg.contains("unexpected extra")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        match parse("diagram export mermaid --table") {
            Command::Unknown(msg) => assert!(msg.contains("--table requires")),
            other => panic!("expected Unknown, got {other:?}"),
        }
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
                options: ExportArgs::default(),
            }
        );
        // Trailing whitespace gets trimmed but interior spaces survive.
        assert_eq!(
            parse("export json   /tmp/two words.json   "),
            Command::Export {
                format: "json".into(),
                path: "/tmp/two words.json".into(),
                options: ExportArgs::default(),
            }
        );

        // parquet + markdown with their flags.
        use crate::export::ParquetCompression;
        assert_eq!(
            parse("export parquet out.parquet --compression zstd"),
            Command::Export {
                format: "parquet".into(),
                path: "out.parquet".into(),
                options: ExportArgs {
                    compression: Some(ParquetCompression::Zstd),
                    no_truncate: false,
                },
            }
        );
        assert_eq!(
            parse("export markdown out.md --no-truncate"),
            Command::Export {
                format: "markdown".into(),
                path: "out.md".into(),
                options: ExportArgs {
                    compression: None,
                    no_truncate: true,
                },
            }
        );
        // `md` alias + path with a space still works under flags.
        assert_eq!(
            parse("export md /tmp/my report.md --no-truncate"),
            Command::Export {
                format: "md".into(),
                path: "/tmp/my report.md".into(),
                options: ExportArgs {
                    compression: None,
                    no_truncate: true,
                },
            }
        );
        // Both flags at once.
        assert_eq!(
            parse("export parquet out.parquet --compression none"),
            Command::Export {
                format: "parquet".into(),
                path: "out.parquet".into(),
                options: ExportArgs {
                    compression: Some(ParquetCompression::None),
                    no_truncate: false,
                },
            }
        );
        // Bad codec surfaces a friendly error rather than crashing.
        match parse("export parquet out.parquet --compression brotli") {
            Command::Unknown(msg) => assert!(msg.contains("brotli")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        // Bare `--compression` without a codec is rejected.
        match parse("export parquet out.parquet --compression") {
            Command::Unknown(msg) => assert!(msg.contains("requires a codec")),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn empty_and_unknown() {
        assert_eq!(parse(""), Command::Empty);
        assert_eq!(parse("   "), Command::Empty);
        assert_eq!(parse("zz"), Command::Unknown("zz".into()));
    }

    // parser coverage.
    #[test]
    fn schema_diff_minimal() {
        let cmd = parse("schema-diff prod staging");
        assert_eq!(
            cmd,
            Command::SchemaDiff {
                source: "prod".into(),
                target: "staging".into(),
                dialect: None,
                schema: None,
                table: None,
                schema_map: Vec::new(),
            }
        );
    }

    #[test]
    fn schema_diff_with_flags() {
        let cmd = parse(
            "schema-diff prod staging --dialect postgres --schema public \
             --table users --schema-map prod_app=staging_app \
             --schema-map prod_log=staging_log",
        );
        let Command::SchemaDiff {
            source,
            target,
            dialect,
            schema,
            table,
            schema_map,
        } = cmd
        else {
            panic!("expected SchemaDiff, got {cmd:?}");
        };
        assert_eq!(source, "prod");
        assert_eq!(target, "staging");
        assert_eq!(dialect.as_deref(), Some("postgres"));
        assert_eq!(schema.as_deref(), Some("public"));
        assert_eq!(table.as_deref(), Some("users"));
        assert_eq!(
            schema_map,
            vec![
                ("prod_app".to_owned(), "staging_app".to_owned()),
                ("prod_log".to_owned(), "staging_log".to_owned()),
            ]
        );
    }

    #[test]
    fn schema_diff_alias_resolves() {
        // `schemadiff` (no hyphen) routes through the same parser.
        let cmd = parse("schemadiff src tgt");
        assert!(matches!(cmd, Command::SchemaDiff { .. }));
    }

    #[test]
    fn schema_diff_missing_target_is_unknown() {
        let cmd = parse("schema-diff onlyone");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("expected two connection names"), "got: {msg}");
    }

    #[test]
    fn schema_diff_malformed_map_is_unknown() {
        let cmd = parse("schema-diff a b --schema-map noequals");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("--schema-map expects"), "got: {msg}");
    }

    #[test]
    fn schema_diff_unknown_flag_is_rejected() {
        let cmd = parse("schema-diff a b --bogus");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("unknown flag"), "got: {msg}");
    }

    #[test]
    fn chart_bar_defaults() {
        assert_eq!(
            parse("chart bar"),
            Command::Chart(ChartArg::On {
                kind: ChartKindArg::Bar,
                title: None,
                x_col: None,
                y_col: None,
            })
        );
    }

    #[test]
    fn chart_off_dismisses() {
        assert_eq!(parse("chart off"), Command::Chart(ChartArg::Off));
        assert_eq!(parse("chart none"), Command::Chart(ChartArg::Off));
        assert_eq!(parse("chart hide"), Command::Chart(ChartArg::Off));
    }

    #[test]
    fn chart_off_rejects_extra_args() {
        let cmd = parse("chart off bogus");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("no extra arguments"), "got: {msg}");
    }

    #[test]
    fn chart_line_with_x_and_y_overrides() {
        assert_eq!(
            parse("chart line --x ts --y revenue"),
            Command::Chart(ChartArg::On {
                kind: ChartKindArg::Line,
                title: None,
                x_col: Some("ts".into()),
                y_col: Some("revenue".into()),
            })
        );
    }

    #[test]
    fn chart_sparkline_col_alias() {
        assert_eq!(
            parse("chart sparkline --col revenue"),
            Command::Chart(ChartArg::On {
                kind: ChartKindArg::Sparkline,
                title: None,
                x_col: None,
                y_col: Some("revenue".into()),
            })
        );
        // Short alias `spark` is also accepted.
        assert_eq!(
            parse("chart spark -c revenue"),
            Command::Chart(ChartArg::On {
                kind: ChartKindArg::Sparkline,
                title: None,
                x_col: None,
                y_col: Some("revenue".into()),
            })
        );
    }

    #[test]
    fn chart_title_flag() {
        assert_eq!(
            parse("chart bar --title sales"),
            Command::Chart(ChartArg::On {
                kind: ChartKindArg::Bar,
                title: Some("sales".into()),
                x_col: None,
                y_col: None,
            })
        );
    }

    #[test]
    fn chart_requires_subcommand() {
        let cmd = parse("chart");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("subcommand required"), "got: {msg}");
    }

    #[test]
    fn chart_unknown_kind_is_rejected() {
        let cmd = parse("chart pie");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("unknown subcommand"), "got: {msg}");
    }

    #[test]
    fn chart_unknown_flag_is_rejected() {
        let cmd = parse("chart bar --bogus value");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("unknown flag"), "got: {msg}");
    }

    #[test]
    fn chart_flag_without_value_errors() {
        let cmd = parse("chart bar --x");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("--x requires"), "got: {msg}");
    }

    #[test]
    fn pivot_minimal_rows_only() {
        assert_eq!(
            parse("pivot rows=country"),
            Command::Pivot(PivotArg::On {
                rows: vec!["country".into()],
                cols: None,
                value: None,
                agg: PivotAggArg::Count,
            })
        );
    }

    #[test]
    fn pivot_full_spec() {
        assert_eq!(
            parse("pivot rows=country,year cols=segment value=revenue agg=sum"),
            Command::Pivot(PivotArg::On {
                rows: vec!["country".into(), "year".into()],
                cols: Some("segment".into()),
                value: Some("revenue".into()),
                agg: PivotAggArg::Sum,
            })
        );
    }

    #[test]
    fn pivot_off_aliases() {
        assert_eq!(parse("pivot off"), Command::Pivot(PivotArg::Off));
        assert_eq!(parse("pivot none"), Command::Pivot(PivotArg::Off));
        assert_eq!(parse("pivot hide"), Command::Pivot(PivotArg::Off));
    }

    #[test]
    fn pivot_off_rejects_extra_args() {
        let cmd = parse("pivot off bogus");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("no extra arguments"), "got: {msg}");
    }

    #[test]
    fn pivot_requires_dim() {
        let cmd = parse("pivot agg=sum value=revenue");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("rows= or cols="), "got: {msg}");
    }

    #[test]
    fn pivot_sum_requires_value() {
        let cmd = parse("pivot rows=country agg=sum");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("requires value="), "got: {msg}");
    }

    #[test]
    fn pivot_rejects_unknown_agg() {
        let cmd = parse("pivot rows=k agg=median");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("unknown aggregator"), "got: {msg}");
    }

    #[test]
    fn pivot_rejects_unknown_key() {
        let cmd = parse("pivot rows=k foo=bar");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("unknown key"), "got: {msg}");
    }

    #[test]
    fn pivot_rejects_malformed_token() {
        let cmd = parse("pivot rows");
        let Command::Unknown(msg) = cmd else {
            panic!("expected Unknown, got {cmd:?}");
        };
        assert!(msg.contains("key=value"), "got: {msg}");
    }

    #[test]
    fn pivot_aggregator_aliases() {
        for (token, expected) in [
            ("sum", PivotAggArg::Sum),
            ("total", PivotAggArg::Sum),
            ("mean", PivotAggArg::Avg),
            ("n", PivotAggArg::Count),
        ] {
            assert_eq!(PivotAggArg::from_token(token), Some(expected));
        }
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
