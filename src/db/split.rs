//! Splits editor text into SQL statements on `;`, ignoring semicolons inside
//! quotes, comments and Postgres dollar-quoted bodies. SQL Server text is
//! first cut into `GO` batches (see `split_mssql`).

use super::Driver;

/// Editor text as the statements `driver` runs one by one.
pub fn statements(driver: Driver, sql: &str) -> Vec<String> {
    match driver {
        Driver::Mssql => split_mssql(sql),
        _ => split_statements(sql, driver == Driver::MySql),
    }
}

/// What joins statements back into text that `statements` splits the
/// same way: T-SQL needs `GO`, since a `;` doesn't end a procedure body.
pub fn separator(driver: Driver) -> &'static str {
    match driver {
        Driver::Mssql => "\nGO\n",
        _ => ";\n",
    }
}

/// `backslash_escapes`: MySQL treats `\'` inside quotes as an escaped quote.
/// Standard SQL (Postgres, SQLite) doesn't, so `'C:\'` is a complete string
/// there; Postgres `E'…'` strings are the exception and always escape.
pub fn split_statements(sql: &str, backslash_escapes: bool) -> Vec<String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\'' | '"' | '`' => {
                let e_string = c == '\''
                    && i > 0
                    && matches!(chars[i - 1], 'e' | 'E')
                    && (i < 2 || !(chars[i - 2].is_alphanumeric() || chars[i - 2] == '_'));
                let escapes = c != '"' && (backslash_escapes || e_string);
                cur.push(c);
                i += 1;
                while i < chars.len() {
                    cur.push(chars[i]);
                    if chars[i] == c {
                        // doubled quote is an escape
                        if chars.get(i + 1) == Some(&c) {
                            cur.push(c);
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    if chars[i] == '\\' && escapes {
                        if let Some(&n) = chars.get(i + 1) {
                            cur.push(n);
                            i += 1;
                        }
                    }
                    i += 1;
                }
            }
            '-' if next == Some('-') => {
                while i < chars.len() && chars[i] != '\n' {
                    cur.push(chars[i]);
                    i += 1;
                }
                continue;
            }
            '/' if next == Some('*') => {
                cur.push_str("/*");
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    cur.push(chars[i]);
                    i += 1;
                }
                if i < chars.len() {
                    cur.push_str("*/");
                    i += 1;
                }
            }
            '$' => {
                // $tag$ ... $tag$
                let mut j = i + 1;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                if j < chars.len() && chars[j] == '$' {
                    let tag: String = chars[i..=j].iter().collect();
                    cur.push_str(&tag);
                    i = j + 1;
                    let tag_chars: Vec<char> = tag.chars().collect();
                    while i < chars.len() {
                        if chars[i..].starts_with(&tag_chars) {
                            cur.push_str(&tag);
                            i += tag_chars.len() - 1;
                            break;
                        }
                        cur.push(chars[i]);
                        i += 1;
                    }
                } else {
                    cur.push(c);
                }
            }
            ';' => {
                push_stmt(&mut out, &cur);
                cur.clear();
            }
            _ => cur.push(c),
        }
        i += 1;
    }
    push_stmt(&mut out, &cur);
    out
}

fn push_stmt(out: &mut Vec<String>, s: &str) {
    let t = s.trim();
    if !t.is_empty() && !is_only_comments(t) {
        out.push(t.to_string());
    }
}

fn is_only_comments(s: &str) -> bool {
    s.lines().all(|l| {
        let l = l.trim();
        l.is_empty() || l.starts_with("--")
    })
}

/// T-SQL: `GO` lines end a batch (as in SSMS and sqlcmd). A batch is then
/// split on `;` so each statement gets its own result, unless splitting
/// would change what it means: variables (`DECLARE`), blocks (`BEGIN …
/// END`, `IF` / `WHILE`) and routine bodies (`CREATE PROCEDURE …`) stay
/// together as one batch.
pub fn split_mssql(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    for batch in go_batches(sql) {
        if keeps_whole(&batch) {
            let body = batch.trim().trim_end_matches(';');
            push_stmt(&mut out, body);
        } else {
            out.extend(split_statements(&batch, false));
        }
    }
    out
}

/// The text between `GO` lines. A `GO` inside a string or block comment
/// doesn't count; `GO 5` (repeat) is read as a plain `GO`.
pub fn go_batches(sql: &str) -> Vec<String> {
    let mut batches = vec![String::new()];
    let (mut in_string, mut in_comment) = (false, false);
    for line in sql.split_inclusive('\n') {
        let at_code = !in_string && !in_comment;
        if at_code && is_go_line(line) {
            batches.push(String::new());
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let (c, next) = (chars[i], chars.get(i + 1).copied());
            match (in_string, in_comment) {
                (true, _) if c == '\'' => in_string = false,
                (_, true) if c == '*' && next == Some('/') => {
                    in_comment = false;
                    i += 1;
                }
                (false, false) if c == '\'' => in_string = true,
                (false, false) if c == '/' && next == Some('*') => {
                    in_comment = true;
                    i += 1;
                }
                (false, false) if c == '-' && next == Some('-') => break,
                _ => {}
            }
            i += 1;
        }
        if let Some(batch) = batches.last_mut() {
            batch.push_str(line);
        }
    }
    batches
}

fn is_go_line(line: &str) -> bool {
    let code = line.split("--").next().unwrap_or_default().trim();
    let code = code.trim_end_matches(';').trim_end();
    let mut words = code.split_whitespace();
    words.next().is_some_and(|w| w.eq_ignore_ascii_case("go"))
        && words
            .next()
            .is_none_or(|count| count.bytes().all(|b| b.is_ascii_digit()))
        && words.next().is_none()
}

/// Lower-case words of T-SQL code, leaving out strings, comments and
/// quoted identifiers.
fn code_words(sql: &str) -> Vec<String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut i = 0;
    while i < chars.len() {
        let (c, next) = (chars[i], chars.get(i + 1).copied());
        let closing = match c {
            '\'' => Some('\''),
            '"' => Some('"'),
            '[' => Some(']'),
            _ => None,
        };
        if c.is_alphanumeric() || matches!(c, '_' | '@' | '#') {
            word.push(c.to_ascii_lowercase());
            i += 1;
            continue;
        }
        if !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        if let Some(close) = closing {
            i += 1;
            while i < chars.len() && chars[i] != close {
                i += 1;
            }
        } else if c == '-' && next == Some('-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 1;
        }
        i += 1;
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Whether a T-SQL batch would mean something else split on `;`.
fn keeps_whole(batch: &str) -> bool {
    let words = code_words(batch);
    words.iter().enumerate().any(|(at, word)| {
        let after = |n: usize| words.get(at + n).map(String::as_str);
        match word.as_str() {
            "declare" | "while" => true,
            // IF starts a block, but not in DROP TABLE IF EXISTS (or DROP
            // COLUMN IF EXISTS…).
            "if" => at < 2 || words[at - 2] != "drop",
            // BEGIN TRAN starts a transaction; any other BEGIN a block.
            "begin" => !matches!(after(1), Some("tran" | "transaction" | "distributed")),
            "create" | "alter" => {
                let kind = match (after(1), after(2)) {
                    (Some("or"), Some("alter")) => after(3),
                    (kind, _) => kind,
                };
                matches!(
                    kind,
                    Some("proc" | "procedure" | "function" | "trigger" | "view")
                )
            }
            _ => false,
        }
    })
}

/// The statement the cursor at byte `offset` is in, for ⌘↵: each statement
/// owns the text from its start up to where the next one starts, so a
/// cursor just after its `;` (or on the blank lines below) still runs it.
pub fn statement_at(sql: &str, offset: usize, driver: Driver) -> Option<String> {
    let stmts = statements(driver, sql);
    let mut from = 0;
    let mut starts = Vec::with_capacity(stmts.len());
    for s in &stmts {
        let at = sql[from..].find(s.as_str())? + from;
        starts.push(at);
        from = at + s.len();
    }
    let i = starts.iter().rposition(|&st| st <= offset).unwrap_or(0);
    stmts.get(i).cloned()
}

#[cfg(test)]
mod tests {
    use super::{split_mssql, split_statements, statement_at};
    use crate::db::Driver;

    #[test]
    fn splits_respecting_quotes_and_dollar_bodies() {
        let sql = "select ';'; -- c;\nselect 2 /* ; */;\ncreate function f() returns int as $$ select 1; $$ language sql;";
        let s = split_statements(sql, false);
        assert_eq!(s.len(), 3, "{s:?}");
        assert_eq!(s[0], "select ';'");
        assert!(s[2].contains("$$ select 1; $$"));
    }

    #[test]
    fn skips_empty_and_comment_only() {
        assert_eq!(
            split_statements(" ; -- hi\n ;select 1", false),
            vec!["select 1"]
        );
    }

    #[test]
    fn backslash_is_literal_except_in_mysql_and_e_strings() {
        // Postgres / SQLite: a trailing backslash doesn't escape the quote.
        assert_eq!(
            split_statements(r"select 'C:\'; select 2", false),
            [r"select 'C:\'", "select 2"]
        );
        // MySQL: \' is an escaped quote, so the string continues.
        assert_eq!(
            split_statements(r"select 'it\'s; ok'; select 2", true).len(),
            2
        );
        // Postgres E'' strings escape; a column named `name` before a quote doesn't count.
        assert_eq!(
            split_statements(r"select E'a\'; b'; select 2", false).len(),
            2
        );
        assert_eq!(
            split_statements(r"select name'x\'; select 2", false).len(),
            2
        );
    }

    #[test]
    fn statement_under_the_cursor() {
        let sql = "select 1;\n\nselect 'a;b'\nfrom t;  \n-- note\nselect 3";
        let at = |needle: &str| sql.find(needle).unwrap();
        assert_eq!(
            statement_at(sql, 0, Driver::Postgres).as_deref(),
            Some("select 1")
        );
        // Just after the `;`, and on the blank line below: still the first.
        assert_eq!(
            statement_at(sql, at(";") + 1, Driver::Postgres).as_deref(),
            Some("select 1")
        );
        assert_eq!(
            statement_at(sql, at("\n\n") + 1, Driver::Postgres).as_deref(),
            Some("select 1")
        );
        assert_eq!(
            statement_at(sql, at("from"), Driver::Postgres).as_deref(),
            Some("select 'a;b'\nfrom t")
        );
        assert_eq!(
            statement_at(sql, sql.len(), Driver::Postgres).as_deref(),
            Some("-- note\nselect 3")
        );
        assert_eq!(statement_at("", 0, Driver::Postgres), None);
    }

    #[test]
    fn tsql_batches_split_on_go_and_keep_blocks_whole() {
        let sql = "select 1; select 'a;b'\ngo\ncreate procedure p as\nbegin\n  select 1;\n  select 2;\nend\nGO 2\n\
                   declare @n int = 1; select @n\n-- go\nselect '\ngo\n'";
        let s = split_mssql(sql);
        assert_eq!(s.len(), 4, "{s:?}");
        assert_eq!(s[0], "select 1");
        assert_eq!(s[1], "select 'a;b'");
        assert!(
            s[2].starts_with("create procedure p") && s[2].ends_with("end"),
            "{s:?}"
        );
        // DECLARE: one batch, and a GO inside a string isn't a separator.
        assert!(
            s[3].starts_with("declare @n") && s[3].ends_with("'\ngo\n'"),
            "{s:?}"
        );
        assert_eq!(
            split_mssql("drop table if exists t; create table t (id int)"),
            ["drop table if exists t", "create table t (id int)"]
        );
        assert_eq!(
            split_mssql("if exists (select 1) drop table t; select 1").len(),
            1
        );
        // BEGIN TRAN isn't a block.
        assert_eq!(
            split_mssql("begin tran; update t set a = 1; commit"),
            ["begin tran", "update t set a = 1", "commit"]
        );
        assert_eq!(
            statement_at("select 1\nGO\nselect 2", 12, Driver::Mssql).as_deref(),
            Some("select 2")
        );
    }
}
