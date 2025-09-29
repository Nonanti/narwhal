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
    Export { format: String, path: String },
    Help,
    Unknown(String),
    Empty,
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
        "export" => parse_export(arg),
        "help" | "h" => Command::Help,
        _ => Command::Unknown(trimmed.to_owned()),
    }
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
