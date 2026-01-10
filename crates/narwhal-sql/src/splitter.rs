use serde::{Deserialize, Serialize};

/// SQL dialect understood by the splitter.
///
/// The dialect affects how string literals and identifiers are escaped and
/// whether dialect-specific quoting (PostgreSQL dollar-quoted strings,
/// MySQL backtick identifiers) is recognised.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Dialect {
    /// PostgreSQL: recognises `$tag$ ... $tag$` and standard SQL escapes.
    Postgres,
    /// SQLite: standard SQL escapes only.
    Sqlite,
    /// MySQL: backtick identifiers in addition to standard SQL escapes.
    MySql,
    /// Conservative default: standard SQL only.
    #[default]
    Generic,
}

/// A single statement located inside a larger SQL source.
///
/// `text` is the statement with surrounding whitespace trimmed; `start` and
/// `end` are byte offsets into the original source that bracket the
/// statement (terminating semicolon, when present, is included).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}

/// Split `source` into statements using the default dialect.
#[must_use]
pub fn split(source: &str) -> Vec<Statement<'_>> {
    split_with(source, Dialect::default())
}

/// Split `source` into statements using `dialect`-specific quoting rules.
#[must_use]
pub fn split_with(source: &str, dialect: Dialect) -> Vec<Statement<'_>> {
    Splitter::new(source, dialect).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    LineComment,
    BlockComment(u32),
    /// Single-quoted string literal. `backslash_escape` controls whether
    /// a `\` consumes the next byte (MySQL default mode, PostgreSQL
    /// `E'...'` escape strings). Standard SQL and PG plain literals use
    /// `false` and treat `\` as an ordinary character.
    StringLiteral {
        backslash_escape: bool,
    },
    QuotedIdentifier,
    Backtick,
}

struct Splitter<'a> {
    source: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// Byte offset of the first non-whitespace character of the current
    /// statement, or `None` when the splitter has not seen any content yet.
    start: Option<usize>,
    dialect: Dialect,
}

impl<'a> Splitter<'a> {
    fn new(source: &'a str, dialect: Dialect) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            pos: 0,
            start: None,
            dialect,
        }
    }

    fn peek(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn current(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    /// Decide whether the literal opening at `self.pos` should honour
    /// backslash escapes. MySQL applies them unconditionally to single-quoted
    /// strings; PostgreSQL only inside `E'...'` (`e'...'`) where the prefix
    /// stands as its own token.
    fn opening_uses_backslash_escape(&self) -> bool {
        match self.dialect {
            Dialect::MySql => true,
            Dialect::Postgres => self.postgres_e_prefix_at_open(),
            Dialect::Sqlite | Dialect::Generic => false,
        }
    }

    /// True when the byte immediately before the current `'` is an `E`/`e`
    /// that is itself preceded by a non-identifier byte (or start of input),
    /// i.e. the `E` is the entire previous token rather than the tail of
    /// some identifier like `name`.
    fn postgres_e_prefix_at_open(&self) -> bool {
        if self.pos == 0 {
            return false;
        }
        let prev = self.bytes[self.pos - 1];
        if prev != b'E' && prev != b'e' {
            return false;
        }
        if self.pos < 2 {
            return true;
        }
        let before = self.bytes[self.pos - 2];
        !(before.is_ascii_alphanumeric() || before == b'_')
    }

    /// Tries to recognise a dollar-quote opener at the current position and
    /// returns the tag length (including dollars) when found. The opener
    /// follows the grammar `\$[A-Za-z_][A-Za-z0-9_]*\$` or simply `\$\$`.
    fn match_dollar_tag(&self) -> Option<usize> {
        if self.dialect != Dialect::Postgres {
            return None;
        }
        if self.current() != Some(b'$') {
            return None;
        }
        let mut i = self.pos + 1;
        // Inner tag: letters/underscore followed by letters/digits/underscore.
        let mut have_inner = false;
        if let Some(&first) = self.bytes.get(i) {
            if first.is_ascii_alphabetic() || first == b'_' {
                have_inner = true;
                i += 1;
                while let Some(&c) = self.bytes.get(i) {
                    if c.is_ascii_alphanumeric() || c == b'_' {
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
        }
        if self.bytes.get(i) == Some(&b'$') {
            // Empty tag `$$` is permitted; tag identifier optional.
            let _ = have_inner;
            Some(i - self.pos + 1)
        } else {
            None
        }
    }

    /// Given a dollar-quote opening at `self.pos` of length `tag_len`, find
    /// the matching closing tag and return the position past the closing
    /// dollar. Returns `None` when the source ends without a match.
    ///
    /// Uses [`memchr::memmem`] so long PL/pgSQL bodies don't see a hot
    /// O(n) byte-by-byte scan (L3).
    fn find_dollar_close(&self, tag_len: usize) -> Option<usize> {
        let tag = &self.bytes[self.pos..self.pos + tag_len];
        let haystack = &self.bytes[self.pos + tag_len..];
        memchr::memmem::find(haystack, tag).map(|offset| self.pos + tag_len + offset + tag_len)
    }

    fn emit(&mut self, end: usize) -> Option<Statement<'a>> {
        let start = self.start.take()?;
        let trimmed_end = self.source[start..end].trim_end().len() + start;
        let raw = self.source[start..trimmed_end].trim_start();
        if raw.is_empty() {
            return None;
        }
        let new_start = trimmed_end - raw.len();
        Some(Statement {
            text: raw,
            start: new_start,
            end: trimmed_end,
        })
    }
}

impl<'a> Iterator for Splitter<'a> {
    type Item = Statement<'a>;

    #[allow(clippy::too_many_lines)]
    fn next(&mut self) -> Option<Statement<'a>> {
        let mut state = State::Normal;

        while self.pos < self.bytes.len() {
            let byte = self.bytes[self.pos];

            match state {
                State::Normal => {
                    // Comment openers are treated like whitespace: they do
                    // not begin a statement on their own.
                    if byte == b'-' && self.peek(1) == Some(b'-') {
                        state = State::LineComment;
                        self.pos += 2;
                        continue;
                    }
                    if byte == b'/' && self.peek(1) == Some(b'*') {
                        state = State::BlockComment(1);
                        self.pos += 2;
                        continue;
                    }

                    // Track the first non-whitespace byte as statement start.
                    if !byte.is_ascii_whitespace() && self.start.is_none() {
                        self.start = Some(self.pos);
                    }

                    if byte == b'\'' {
                        let backslash_escape = self.opening_uses_backslash_escape();
                        state = State::StringLiteral { backslash_escape };
                        self.pos += 1;
                        continue;
                    }
                    if byte == b'"' {
                        state = State::QuotedIdentifier;
                        self.pos += 1;
                        continue;
                    }
                    if byte == b'`' && self.dialect == Dialect::MySql {
                        state = State::Backtick;
                        self.pos += 1;
                        continue;
                    }
                    if byte == b'$' {
                        if let Some(tag_len) = self.match_dollar_tag() {
                            if let Some(end) = self.find_dollar_close(tag_len) {
                                self.pos = end;
                            } else {
                                // Unterminated dollar quote: consume to the
                                // end of input and let the engine surface the
                                // syntax error.
                                self.pos = self.bytes.len();
                            }
                            continue;
                        }
                    }
                    if byte == b';' {
                        let end = self.pos + 1;
                        self.pos = end;
                        if let Some(stmt) = self.emit(end) {
                            return Some(stmt);
                        }
                        continue;
                    }
                    self.pos += 1;
                }
                State::LineComment => {
                    if byte == b'\n' {
                        state = State::Normal;
                    }
                    self.pos += 1;
                }
                State::BlockComment(depth) => {
                    if byte == b'/' && self.peek(1) == Some(b'*') {
                        state = State::BlockComment(depth + 1);
                        self.pos += 2;
                        continue;
                    }
                    if byte == b'*' && self.peek(1) == Some(b'/') {
                        self.pos += 2;
                        state = if depth == 1 {
                            State::Normal
                        } else {
                            State::BlockComment(depth - 1)
                        };
                        continue;
                    }
                    self.pos += 1;
                }
                State::StringLiteral { backslash_escape } => {
                    if backslash_escape && byte == b'\\' {
                        // Skip the escape byte and whatever follows. If the
                        // input ends right after a backslash the outer loop
                        // condition handles termination.
                        self.pos += if self.peek(1).is_some() { 2 } else { 1 };
                        continue;
                    }
                    if byte == b'\'' {
                        if self.peek(1) == Some(b'\'') {
                            // Escaped single quote inside the literal
                            // (`''` is the SQL-standard escape and works
                            // in every dialect).
                            self.pos += 2;
                            continue;
                        }
                        state = State::Normal;
                    }
                    self.pos += 1;
                }
                State::QuotedIdentifier => {
                    if byte == b'"' {
                        if self.peek(1) == Some(b'"') {
                            self.pos += 2;
                            continue;
                        }
                        state = State::Normal;
                    }
                    self.pos += 1;
                }
                State::Backtick => {
                    if byte == b'`' {
                        state = State::Normal;
                    }
                    self.pos += 1;
                }
            }
        }

        let end = self.bytes.len();
        self.emit(end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(input: &str, dialect: Dialect) -> Vec<&str> {
        split_with(input, dialect)
            .into_iter()
            .map(|s| s.text)
            .collect()
    }

    #[test]
    fn single_statement_without_terminator() {
        assert_eq!(texts("SELECT 1", Dialect::Generic), vec!["SELECT 1"]);
    }

    #[test]
    fn two_statements_separated_by_semicolon() {
        assert_eq!(
            texts("SELECT 1; SELECT 2;", Dialect::Generic),
            vec!["SELECT 1;", "SELECT 2;"]
        );
    }

    #[test]
    fn trailing_whitespace_is_trimmed() {
        let stmts = split_with("  SELECT 1  ;  SELECT 2  ", Dialect::Generic);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].text, "SELECT 1  ;");
        assert_eq!(stmts[1].text, "SELECT 2");
    }

    #[test]
    fn semicolon_inside_string_literal_does_not_split() {
        assert_eq!(
            texts("SELECT 'a;b'; SELECT 2", Dialect::Generic),
            vec!["SELECT 'a;b';", "SELECT 2"]
        );
    }

    #[test]
    fn escaped_quote_inside_string_literal() {
        assert_eq!(
            texts("SELECT 'it''s ok'; SELECT 2", Dialect::Generic),
            vec!["SELECT 'it''s ok';", "SELECT 2"]
        );
    }

    #[test]
    fn line_comment_swallows_semicolon() {
        assert_eq!(
            texts("SELECT 1 -- ignore;\n; SELECT 2", Dialect::Generic),
            vec!["SELECT 1 -- ignore;\n;", "SELECT 2"]
        );
    }

    #[test]
    fn nested_block_comment() {
        assert_eq!(
            texts("SELECT 1 /* a /* b */ c */; SELECT 2", Dialect::Generic),
            vec!["SELECT 1 /* a /* b */ c */;", "SELECT 2"]
        );
    }

    #[test]
    fn quoted_identifier_with_semicolon() {
        assert_eq!(
            texts(r#"SELECT "a;b"; SELECT 2"#, Dialect::Generic),
            vec![r#"SELECT "a;b";"#, "SELECT 2"]
        );
    }

    #[test]
    fn postgres_dollar_quote_anonymous() {
        assert_eq!(
            texts("SELECT $$hello;world$$; SELECT 2", Dialect::Postgres),
            vec!["SELECT $$hello;world$$;", "SELECT 2"]
        );
    }

    #[test]
    fn postgres_dollar_quote_with_tag() {
        assert_eq!(
            texts("SELECT $tag$body;more$tag$; SELECT 2", Dialect::Postgres),
            vec!["SELECT $tag$body;more$tag$;", "SELECT 2"]
        );
    }

    #[test]
    fn dollar_quote_ignored_outside_postgres() {
        // In Generic dialect dollar signs are ordinary punctuation, so the
        // first semicolon splits the statement.
        let stmts = texts("SELECT $$x;y$$", Dialect::Generic);
        assert_eq!(stmts, vec!["SELECT $$x;", "y$$"]);
    }

    #[test]
    fn empty_input_yields_no_statements() {
        assert!(split_with("", Dialect::Generic).is_empty());
        assert!(split_with("   \n  ", Dialect::Generic).is_empty());
    }

    #[test]
    fn standalone_comment_yields_no_statements() {
        assert!(split_with("-- nothing here", Dialect::Generic).is_empty());
        assert!(split_with("/* nothing */", Dialect::Generic).is_empty());
    }

    #[test]
    fn mysql_backslash_escapes_quote_in_string_literal() {
        // In MySQL the default SQL mode does NOT include NO_BACKSLASH_ESCAPES,
        // so `\'` inside a string literal is an escaped quote that does not
        // close the literal. The next `'` closes it.
        // Input SQL: INSERT INTO t VALUES ('\'); SELECT 1
        //   - Standard splitter (wrong for MySQL): sees `'\'` as a 3-char
        //     literal closing at byte 7, then the `;` ends the statement,
        //     leaving `'; SELECT 1` as broken second statement.
        //   - MySQL-aware splitter (this test): `\'` is escaped quote,
        //     literal never closes before EOF → the whole input is one
        //     (syntactically incomplete) statement.
        let input = r"INSERT INTO t VALUES ('\'); SELECT 1";
        let stmts = texts(input, Dialect::MySql);
        assert_eq!(stmts.len(), 1, "expected 1 statement, got: {stmts:?}");
        assert_eq!(stmts[0], input);
    }

    #[test]
    fn mysql_backslash_escapes_backslash_in_string_literal() {
        // `\\` is a literal backslash; the literal is closed by the
        // following `'`. The semicolon then splits cleanly.
        let input = r"SELECT '\\'; SELECT 2";
        assert_eq!(
            texts(input, Dialect::MySql),
            vec![r"SELECT '\\';", "SELECT 2"]
        );
    }

    #[test]
    fn generic_dialect_does_not_treat_backslash_as_escape() {
        // Regression: in Generic/Postgres (non-E) dialect, `\` is an
        // ordinary character. `'\'` is a complete 3-char literal.
        let input = r"INSERT INTO t VALUES ('\'); SELECT 1";
        let stmts = texts(input, Dialect::Generic);
        assert_eq!(stmts.len(), 2, "expected 2 statements, got: {stmts:?}");
    }

    #[test]
    fn postgres_e_string_treats_backslash_as_escape() {
        // PG `E'...'` (escape-string) honours backslash escapes.
        // `E'\''` => string containing a single `'`; literal closes at the
        // third `'`. The trailing `;` splits the statement.
        let input = r"SELECT E'\''; SELECT 2";
        let stmts = texts(input, Dialect::Postgres);
        assert_eq!(stmts.len(), 2, "expected 2 statements, got: {stmts:?}");
    }

    #[test]
    fn postgres_plain_string_does_not_treat_backslash_as_escape() {
        // Without the `E` prefix PG follows standard SQL: `\` is ordinary.
        // `'\''` => `'\'` literal, then `'` opens another literal that
        // never closes → 1 statement consuming the rest of input.
        let input = r"SELECT '\''; SELECT 2";
        let stmts = texts(input, Dialect::Postgres);
        assert_eq!(stmts.len(), 1, "expected 1 statement, got: {stmts:?}");
    }

    #[test]
    fn postgres_e_prefix_only_recognised_at_token_boundary() {
        // `name='value'` should NOT trigger E-string mode just because an
        // identifier happens to end in `e`. The `e` here is part of the
        // identifier `name`, not an escape-string prefix.
        let input = r"SELECT name='\'' ; SELECT 2";
        let stmts = texts(input, Dialect::Postgres);
        // Standard parsing applies → 1 statement (unterminated literal).
        assert_eq!(stmts.len(), 1, "expected 1 statement, got: {stmts:?}");
    }

    #[test]
    fn offsets_point_to_original_source() {
        let src = "  SELECT 1; SELECT 2;";
        let stmts = split_with(src, Dialect::Generic);
        assert_eq!(&src[stmts[0].start..stmts[0].end], "SELECT 1;");
        assert_eq!(&src[stmts[1].start..stmts[1].end], "SELECT 2;");
    }
}
