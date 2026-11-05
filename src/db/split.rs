//! Splits editor text into SQL statements on `;`, ignoring semicolons inside
//! quotes, comments and Postgres dollar-quoted bodies.

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

#[cfg(test)]
mod tests {
    use super::split_statements;

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
}
