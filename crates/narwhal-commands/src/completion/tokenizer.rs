//! Tiny SQL tokeniser used by the completion engine. Not a full
//! parser; just enough to walk identifiers and keywords.

use super::context::TABLE_EXPECTED_KEYWORDS;
use super::keywords::KEYWORDS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Token {
    /// SQL identifier (table name, column name, etc.).
    Ident(String),
    /// SQL keyword (may also be an identifier in some contexts, but we
    /// classify it as a keyword when it matches a known SQL word).
    Keyword(String),
    /// Standalone dot between two identifiers.
    Dot,
    /// String literal — skipped for context purposes.
    StringLiteral,
    /// Anything else (operators, parentheses, etc.).
    Other,
}

/// Tokenise `input` into a sequence of [`Token`] values. Walks forward
/// through the input, skipping string literals and comments.
pub(super) fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Skip whitespace.
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // `--` line comment — skip to end of line.
        if i + 1 < len && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            i += 2;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // `/* */` block comment.
        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < len {
                i += 2; // skip */
            }
            continue;
        }

        // Single-quoted string literal.
        if bytes[i] == b'\'' {
            i += 1;
            while i < len {
                if bytes[i] == b'\'' {
                    i += 1;
                    // Escaped quote inside string.
                    if i < len && bytes[i] == b'\'' {
                        i += 1;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            tokens.push(Token::StringLiteral);
            continue;
        }

        // Double-quoted identifier / string.
        if bytes[i] == b'"' {
            i += 1;
            while i < len && bytes[i] != b'"' {
                i += 1;
            }
            if i < len {
                i += 1;
            }
            tokens.push(Token::StringLiteral);
            continue;
        }

        // Dot.
        if bytes[i] == b'.' {
            tokens.push(Token::Dot);
            i += 1;
            continue;
        }

        // Identifier or keyword.
        if is_ident_start(bytes[i]) {
            let start = i;
            i += 1;
            while i < len && is_ident_cont(bytes[i]) {
                i += 1;
            }
            let word = &input[start..i];
            if TABLE_EXPECTED_KEYWORDS
                .iter()
                .any(|k| k.eq_ignore_ascii_case(word))
                || KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(word))
            {
                tokens.push(Token::Keyword(word.to_ascii_uppercase()));
            } else {
                tokens.push(Token::Ident(word.to_owned()));
            }
            continue;
        }

        // Anything else.
        i += 1;
        tokens.push(Token::Other);
    }

    tokens
}

const fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

const fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
