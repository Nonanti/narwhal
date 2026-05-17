use serde::{Deserialize, Serialize};

/// SQL dialect understood by the splitter.
///
/// The dialect affects how string literals and identifiers are escaped and
/// whether dialect-specific quoting (PostgreSQL dollar-quoted strings,
/// MySQL backtick identifiers) is recognised.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    StringLiteral,
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
    fn find_dollar_close(&self, tag_len: usize) -> Option<usize> {
        let tag = &self.bytes[self.pos..self.pos + tag_len];
        let mut search_from = self.pos + tag_len;
        while search_from + tag_len <= self.bytes.len() {
            if &self.bytes[search_from..search_from + tag_len] == tag {
                return Some(search_from + tag_len);
            }
            search_from += 1;
        }
        None
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
                        state = State::StringLiteral;
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
                State::StringLiteral => {
                    if byte == b'\'' {
                        if self.peek(1) == Some(b'\'') {
                            // Escaped single quote inside the literal.
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
    fn offsets_point_to_original_source() {
        let src = "  SELECT 1; SELECT 2;";
        let stmts = split_with(src, Dialect::Generic);
        assert_eq!(&src[stmts[0].start..stmts[0].end], "SELECT 1;");
        assert_eq!(&src[stmts[1].start..stmts[1].end], "SELECT 2;");
    }
}
