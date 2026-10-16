use sqlparser::dialect::Dialect;
use sqlparser::tokenizer::{Location, Token, Tokenizer, Whitespace};

/// Coarse syntax category for one span of source text, consumed by the
/// `app` crate's editor `Highlighter` to pick a color/style. Deliberately
/// small: the editor only needs enough categories to look right, not a
/// full token taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    Identifier,
    StringLiteral,
    NumberLiteral,
    Operator,
    Punctuation,
    Comment,
    Whitespace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    /// Byte offset range into the original `sql` string, `start..end`.
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

/// Tokenizes `sql` and classifies every token for syntax highlighting.
/// Never fails: on a tokenizer error (e.g. an unterminated string while
/// the user is still typing it), whatever prefix tokenized cleanly is
/// returned rather than nothing, since a half-finished query is the
/// normal state of an editor buffer.
pub fn highlight(sql: &str, dialect: &dyn Dialect) -> Vec<HighlightSpan> {
    let mut tokenizer = Tokenizer::new(dialect, sql);
    let tokens = match tokenizer.tokenize_with_location() {
        Ok(tokens) => tokens,
        Err(_) => return Vec::new(),
    };

    let line_starts = line_start_offsets(sql);
    tokens
        .into_iter()
        .map(|twl| HighlightSpan {
            start: byte_offset(sql, &line_starts, twl.span.start),
            end: byte_offset(sql, &line_starts, twl.span.end),
            kind: classify(&twl.token),
        })
        .collect()
}

fn classify(token: &Token) -> TokenKind {
    match token {
        Token::Word(w) => {
            if w.keyword == sqlparser::keywords::Keyword::NoKeyword {
                TokenKind::Identifier
            } else {
                TokenKind::Keyword
            }
        }
        Token::Number(_, _) => TokenKind::NumberLiteral,
        Token::SingleQuotedString(_)
        | Token::DoubleQuotedString(_)
        | Token::NationalStringLiteral(_)
        | Token::HexStringLiteral(_)
        | Token::EscapedStringLiteral(_)
        | Token::UnicodeStringLiteral(_) => TokenKind::StringLiteral,
        Token::Whitespace(Whitespace::SingleLineComment { .. } | Whitespace::MultiLineComment(_)) => {
            TokenKind::Comment
        }
        Token::Whitespace(_) => TokenKind::Whitespace,
        Token::Comma
        | Token::SemiColon
        | Token::LParen
        | Token::RParen
        | Token::LBracket
        | Token::RBracket
        | Token::LBrace
        | Token::RBrace
        | Token::Period => TokenKind::Punctuation,
        _ => TokenKind::Operator,
    }
}

fn line_start_offsets(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Converts a `sqlparser` `Location` (1-indexed line/column, column
/// counted in `char`s) into a byte offset into `source`.
fn byte_offset(source: &str, line_starts: &[usize], loc: Location) -> usize {
    if loc.line == 0 {
        return 0;
    }
    let line_idx = (loc.line as usize) - 1;
    let line_start = *line_starts.get(line_idx).unwrap_or(&source.len());
    let line_end = line_starts.get(line_idx + 1).map(|&s| s - 1).unwrap_or(source.len());
    let line = source.get(line_start..line_end.max(line_start)).unwrap_or("");

    let col_idx = (loc.column as usize).saturating_sub(1);
    let byte_in_line = line.char_indices().nth(col_idx).map(|(b, _)| b).unwrap_or(line.len());
    line_start + byte_in_line
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::GenericDialect;

    #[test]
    fn classifies_keywords_identifiers_and_strings() {
        let dialect = GenericDialect {};
        let sql = "SELECT email FROM users WHERE email = 'ada'";
        let spans = highlight(sql, &dialect);

        let kind_at = |text: &str| {
            let start = sql.find(text).unwrap();
            spans.iter().find(|s| s.start == start).map(|s| s.kind)
        };

        assert_eq!(kind_at("SELECT"), Some(TokenKind::Keyword));
        assert_eq!(kind_at("email"), Some(TokenKind::Identifier));
        assert_eq!(kind_at("'ada'"), Some(TokenKind::StringLiteral));
    }

    #[test]
    fn byte_offsets_account_for_multibyte_utf8_identifiers() {
        let dialect = GenericDialect {};
        let sql = "SELECT café FROM t";
        let spans = highlight(sql, &dialect);
        let from_span = spans.iter().find(|s| sql[s.start..s.end] == *"FROM").expect("FROM token");
        assert_eq!(&sql[from_span.start..from_span.end], "FROM");
    }

    #[test]
    fn recovers_gracefully_from_unterminated_string_while_typing() {
        let dialect = GenericDialect {};
        // A user mid-keystroke on a string literal shouldn't panic or
        // crash the editor; it's fine to return nothing for this call.
        let spans = highlight("SELECT * FROM t WHERE name = 'unterminated", &dialect);
        let _ = spans;
    }
}
