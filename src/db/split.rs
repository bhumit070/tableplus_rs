//! Splits editor text into SQL statements on `;`, ignoring semicolons inside
//! quotes, comments and Postgres dollar-quoted bodies.

pub fn split_statements(sql: &str) -> Vec<String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\'' | '"' | '`' => {
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
                    if chars[i] == '\\' && c != '"' {
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
        let s = split_statements(sql);
        assert_eq!(s.len(), 3, "{s:?}");
        assert_eq!(s[0], "select ';'");
        assert!(s[2].contains("$$ select 1; $$"));
    }

    #[test]
    fn skips_empty_and_comment_only() {
        assert_eq!(split_statements(" ; -- hi\n ;select 1"), vec!["select 1"]);
    }
}
