//! Syntax highlighting for SQL Server's T-SQL in the query editor. The
//! tree-sitter SQL grammar the other drivers use doesn't know `[names]`,
//! `TOP`, `DECLARE` or `GO`, and one of them breaks the parse for the rest
//! of the script; this lexer colours by token, so nothing after it is lost.

use std::ops::Range;
use std::rc::Rc;

use gpui::{Context, HighlightStyle, SharedString, Window};
use gpui_component::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter,
    InputHighlighterFactory, Rope,
};

/// T-SQL reserved words plus the common non-reserved ones people type.
const KEYWORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "APPLY",
    "AS",
    "ASC",
    "BACKUP",
    "BEGIN",
    "BETWEEN",
    "BREAK",
    "BY",
    "CASCADE",
    "CASE",
    "CATCH",
    "CHECK",
    "CLOSE",
    "COLLATE",
    "COLUMN",
    "COMMIT",
    "CONSTRAINT",
    "CONTINUE",
    "CONVERT",
    "CREATE",
    "CROSS",
    "CURRENT",
    "CURSOR",
    "DATABASE",
    "DEALLOCATE",
    "DECLARE",
    "DEFAULT",
    "DELETE",
    "DENY",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "ESCAPE",
    "EXCEPT",
    "EXEC",
    "EXECUTE",
    "EXISTS",
    "FETCH",
    "FIRST",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "FUNCTION",
    "GO",
    "GOTO",
    "GRANT",
    "GROUP",
    "HAVING",
    "IDENTITY",
    "IF",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "LEFT",
    "LIKE",
    "MATCHED",
    "MAX",
    "MERGE",
    "NEXT",
    "NOCOUNT",
    "NOLOCK",
    "NOT",
    "NULL",
    "OF",
    "OFF",
    "OFFSET",
    "ON",
    "ONLY",
    "OPEN",
    "OPTION",
    "OR",
    "ORDER",
    "OUTER",
    "OUTPUT",
    "OVER",
    "PARTITION",
    "PERCENT",
    "PIVOT",
    "PRIMARY",
    "PRINT",
    "PROC",
    "PROCEDURE",
    "RAISERROR",
    "REFERENCES",
    "RETURN",
    "RETURNS",
    "REVOKE",
    "RIGHT",
    "ROLLBACK",
    "ROW",
    "ROWS",
    "SCHEMA",
    "SELECT",
    "SET",
    "SOURCE",
    "TABLE",
    "TARGET",
    "THEN",
    "THROW",
    "TIES",
    "TOP",
    "TRAN",
    "TRANSACTION",
    "TRIGGER",
    "TRUNCATE",
    "TRY",
    "UNION",
    "UNIQUE",
    "UNPIVOT",
    "UPDATE",
    "USE",
    "USING",
    "VALUES",
    "VIEW",
    "WAITFOR",
    "WHEN",
    "WHERE",
    "WHILE",
    "WITH",
];

const TYPES: &[&str] = &[
    "BIGINT",
    "BINARY",
    "BIT",
    "CHAR",
    "DATE",
    "DATETIME",
    "DATETIME2",
    "DATETIMEOFFSET",
    "DECIMAL",
    "FLOAT",
    "GEOGRAPHY",
    "GEOMETRY",
    "HIERARCHYID",
    "IMAGE",
    "INT",
    "MONEY",
    "NCHAR",
    "NTEXT",
    "NUMERIC",
    "NVARCHAR",
    "REAL",
    "ROWVERSION",
    "SMALLDATETIME",
    "SMALLINT",
    "SMALLMONEY",
    "SQL_VARIANT",
    "SYSNAME",
    "TEXT",
    "TIME",
    "TIMESTAMP",
    "TINYINT",
    "UNIQUEIDENTIFIER",
    "VARBINARY",
    "VARCHAR",
    "XML",
];

/// Keywords that are functions when a `(` follows (`LEFT(name, 3)`).
const FUNCTION_KEYWORDS: &[&str] = &["CONVERT", "LEFT", "RIGHT", "MAX"];

/// After these, the next (possibly dotted) name is a table.
const TABLE_CONTEXT: &[&str] = &["FROM", "JOIN", "INTO", "UPDATE", "TABLE"];

/// Highlight names, in order and not overlapping, for the whole script.
pub fn highlights(sql: &str) -> Vec<(Range<usize>, &'static str)> {
    let mut out = Vec::new();
    let mut table_next = false;
    // The last name seen after a table keyword; coloured once the dotted
    // name ends, so `dbo.orders` colours only `orders`.
    let mut pending_table: Option<Range<usize>> = None;
    let mut at = 0;
    while let Some(c) = sql[at..].chars().next() {
        let start = at;
        let next = sql[at + c.len_utf8()..].chars().next();
        if c.is_whitespace() {
            at += c.len_utf8();
            continue;
        }
        // `dbo.orders`: the part before the dot is the schema, not the table.
        if c == '.' && pending_table.take().is_some() {
            at += 1;
            continue;
        }
        let is_name = c == '['
            || c == '"'
            || (is_word_start(c) && !(matches!(c, 'N' | 'n') && next == Some('\'')));
        // Anything but `.` ends the table name (an alias may follow).
        if let Some(table) = pending_table.take() {
            out.push((table, "type"));
            table_next = false;
        }
        if !is_name && !(c == '-' && next == Some('-')) && !(c == '/' && next == Some('*')) {
            table_next = false;
        }
        match c {
            '-' if next == Some('-') => {
                at = sql[at..].find('\n').map_or(sql.len(), |end| at + end);
                out.push((start..at, "comment"));
            }
            '/' if next == Some('*') => {
                at = block_comment_end(sql, at);
                out.push((start..at, "comment"));
            }
            '\'' => {
                at = quoted_end(sql, at, '\'');
                out.push((start..at, "string"));
            }
            'N' | 'n' if next == Some('\'') => {
                at = quoted_end(sql, at + 1, '\'');
                out.push((start..at, "string"));
            }
            '[' | '"' => {
                at = quoted_end(sql, at, if c == '[' { ']' } else { '"' });
                if table_next {
                    pending_table = Some(start..at);
                }
            }
            '@' => {
                at = word_end(sql, at + 1);
                out.push((start..at, "variable"));
            }
            '0'..='9' => {
                at = number_end(sql, at);
                out.push((start..at, "number"));
            }
            c if is_word_start(c) => {
                at = word_end(sql, at);
                let word = sql[start..at].to_ascii_uppercase();
                let call = sql[at..].trim_start().starts_with('(');
                if TYPES.contains(&word.as_str()) {
                    out.push((start..at, "type"));
                } else if call && FUNCTION_KEYWORDS.contains(&word.as_str()) {
                    out.push((start..at, "function"));
                } else if KEYWORDS.contains(&word.as_str()) {
                    out.push((start..at, "keyword"));
                    table_next = TABLE_CONTEXT.contains(&word.as_str());
                } else if call {
                    out.push((start..at, "function"));
                } else if table_next {
                    pending_table = Some(start..at);
                }
            }
            _ => at += c.len_utf8(),
        }
    }
    if let Some(table) = pending_table {
        out.push((table, "type"));
    }
    out
}

fn is_word_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '#'
}

fn word_end(sql: &str, from: usize) -> usize {
    sql[from..]
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '#' | '$' | '@')))
        .map_or(sql.len(), |len| from + len)
}

/// End of a string or delimited name starting at `from` (its opening
/// character); a doubled closing character is an escaped one.
fn quoted_end(sql: &str, from: usize, close: char) -> usize {
    let mut at = from + 1;
    while let Some(found) = sql[at..].find(close) {
        at += found + 1;
        if !sql[at..].starts_with(close) {
            return at;
        }
        at += 1;
    }
    sql.len()
}

/// T-SQL block comments nest.
fn block_comment_end(sql: &str, from: usize) -> usize {
    let mut depth = 0;
    let mut at = from;
    while at < sql.len() {
        if sql[at..].starts_with("/*") {
            depth += 1;
            at += 2;
        } else if sql[at..].starts_with("*/") {
            depth -= 1;
            at += 2;
            if depth == 0 {
                return at;
            }
        } else {
            at += sql[at..].chars().next().map_or(1, char::len_utf8);
        }
    }
    sql.len()
}

/// `42`, `3.14`, `1e5`, `0x1F`.
fn number_end(sql: &str, from: usize) -> usize {
    let bytes = sql.as_bytes();
    let mut at = from;
    if sql[at..].starts_with("0x") || sql[at..].starts_with("0X") {
        at += 2;
        while bytes.get(at).is_some_and(u8::is_ascii_hexdigit) {
            at += 1;
        }
        return at;
    }
    while bytes
        .get(at)
        .is_some_and(|b| b.is_ascii_digit() || *b == b'.')
    {
        at += 1;
    }
    if bytes.get(at).is_some_and(|b| matches!(b, b'e' | b'E'))
        && bytes
            .get(at + 1)
            .is_some_and(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-'))
    {
        at += 2;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
    }
    at
}

/// The editor's highlighter for SQL Server tabs, replacing tree-sitter.
pub fn factory() -> InputHighlighterFactory {
    Rc::new(|language| {
        Some(Box::new(TsqlHighlighter {
            language: language.to_string().into(),
            highlights: Vec::new(),
        }) as Box<dyn InputHighlighter>)
    })
}

struct TsqlHighlighter {
    language: SharedString,
    highlights: Vec<(Range<usize>, &'static str)>,
}

impl InputHighlighter for TsqlHighlighter {
    fn language(&self) -> SharedString {
        self.language.clone()
    }

    /// Re-lexes the whole script: linear and fast enough on every keystroke
    /// for anything typed into an editor.
    fn update(
        &mut self,
        _edit: Option<InputEdit>,
        text: &Rope,
        _folding: bool,
        _window: &mut Window,
        _cx: &mut Context<EditorState>,
    ) {
        self.highlights = highlights(&text.to_string());
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut runs = Vec::new();
        let mut at = range.start;
        let first = self
            .highlights
            .partition_point(|(r, _)| r.end <= range.start);
        for (token, name) in &self.highlights[first..] {
            if token.start >= range.end {
                break;
            }
            let start = token.start.max(range.start);
            let end = token.end.min(range.end);
            if start > at {
                runs.push((at..start, HighlightStyle::default()));
            }
            runs.push((start..end, resolver.style(name).unwrap_or_default()));
            at = end;
        }
        if at < range.end {
            runs.push((at..range.end, HighlightStyle::default()));
        }
        runs
    }

    fn fold_ranges(&self, _text: &Rope) -> Vec<FoldRange> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (text, highlight name) for each coloured token.
    fn named(sql: &str) -> Vec<(&str, &'static str)> {
        highlights(sql)
            .into_iter()
            .map(|(range, name)| (&sql[range], name))
            .collect()
    }

    #[test]
    fn tsql_script_is_coloured_past_tsql_only_syntax() {
        let sql = "DECLARE @min decimal(10,2) = 40;\n\
                   SELECT TOP (10) [id], [customer] FROM [dbo].[invoices] x\n\
                   WHERE x.[total] > @min AND customer <> N'Ana''s' -- unpaid\n\
                   GO\n\
                   EXEC sp_who; /* outer /* inner */ still */ select getdate(), 0x1F";
        assert_eq!(
            named(sql),
            [
                ("DECLARE", "keyword"),
                ("@min", "variable"),
                ("decimal", "type"),
                ("10", "number"),
                ("2", "number"),
                ("40", "number"),
                ("SELECT", "keyword"),
                ("TOP", "keyword"),
                ("10", "number"),
                ("FROM", "keyword"),
                ("[invoices]", "type"),
                ("WHERE", "keyword"),
                ("@min", "variable"),
                ("AND", "keyword"),
                ("N'Ana''s'", "string"),
                ("-- unpaid", "comment"),
                ("GO", "keyword"),
                ("EXEC", "keyword"),
                ("/* outer /* inner */ still */", "comment"),
                ("select", "keyword"),
                ("getdate", "function"),
                ("0x1F", "number"),
            ]
        );
    }

    #[test]
    fn styles_cover_the_range_without_gaps() {
        let highlighter = TsqlHighlighter {
            language: "sql".into(),
            highlights: highlights("SELECT 1 FROM t"),
        };
        struct Plain;
        impl HighlightStyleResolver for Plain {
            fn style(&self, _: &str) -> Option<HighlightStyle> {
                Some(HighlightStyle::default())
            }
        }
        let runs: Vec<_> = highlighter
            .styles(&(3..14), &Plain)
            .into_iter()
            .map(|(range, _)| range)
            .collect();
        assert_eq!(runs, [3..6, 6..7, 7..8, 8..9, 9..13, 13..14]);
    }
}
