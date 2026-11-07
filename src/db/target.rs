//! Finds the table a query result comes from, so the result can be edited
//! like a table (TablePlus does this for single-table SELECTs).

use super::Driver;

#[derive(Debug, PartialEq)]
enum Tok {
    /// Unquoted word, as written.
    Word(String),
    /// "quoted" / `quoted` / [quoted] identifier, unquoted.
    Quoted(String),
    Sym(char),
}

/// Top-level tokens: anything inside parentheses, strings and comments is
/// skipped, so `extract(year from x)` or a subquery can't be mistaken for
/// the statement's own FROM.
fn top_level_tokens(sql: &str) -> Vec<Tok> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '-' && next == Some('-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && next == Some('*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        if c == '\'' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if let Some(close) = match c {
            '"' => Some('"'),
            '`' => Some('`'),
            '[' => Some(']'),
            _ => None,
        } {
            let start = i + 1;
            i = start;
            while i < chars.len() && chars[i] != close {
                i += 1;
            }
            if depth == 0 {
                out.push(Tok::Quoted(
                    chars[start..i.min(chars.len())].iter().collect(),
                ));
            }
            i += 1;
            continue;
        }
        if c == '(' {
            // Marks a call or subquery at this level, e.g. `FROM f(1)`.
            if depth == 0 {
                out.push(Tok::Sym('('));
            }
            depth += 1;
        } else if c == ')' {
            depth = depth.saturating_sub(1);
        } else if c.is_alphanumeric() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            if depth == 0 {
                out.push(Tok::Word(chars[start..i].iter().collect()));
            }
            continue;
        } else if depth == 0 && !c.is_whitespace() {
            out.push(Tok::Sym(c));
        }
        i += 1;
    }
    out
}

/// Anything that makes rows not map 1:1 to rows of one table.
const NOT_SINGLE_TABLE: &[&str] = &[
    "join",
    "union",
    "intersect",
    "except",
    "group",
    "having",
    "distinct",
    "window",
    "into",
    "lateral",
    "natural",
    "cross",
    "inner",
    "left",
    "right",
    "full",
    "outer",
    "with",
    "values",
];

/// What may follow `FROM table [alias]`.
const AFTER_TABLE: &[&str] = &["where", "order", "limit", "offset", "for", "fetch"];

fn is_kw(t: &Tok, kw: &str) -> bool {
    matches!(t, Tok::Word(w) if w.eq_ignore_ascii_case(kw))
}

/// For `SELECT … FROM <one table> [alias] [WHERE/ORDER BY/LIMIT …]`, the
/// table as (schema, name). Postgres folds unquoted names to lowercase.
pub fn single_table(stmt: &str, driver: Driver) -> Option<(Option<String>, String)> {
    let toks = top_level_tokens(stmt);
    if !is_kw(toks.first()?, "select") {
        return None;
    }
    if toks
        .iter()
        .any(|t| NOT_SINGLE_TABLE.iter().any(|k| is_kw(t, k)))
    {
        return None;
    }
    let from = toks.iter().position(|t| is_kw(t, "from"))?;
    let ident = |t: Option<&Tok>| -> Option<String> {
        match t? {
            Tok::Quoted(s) => Some(s.clone()),
            Tok::Word(w) if !AFTER_TABLE.contains(&w.to_ascii_lowercase().as_str()) => {
                Some(if driver == Driver::Postgres {
                    w.to_lowercase()
                } else {
                    w.clone()
                })
            }
            _ => None,
        }
    };
    let mut i = from + 1;
    let first = ident(toks.get(i))?;
    i += 1;
    let (schema, table) = if toks.get(i) == Some(&Tok::Sym('.')) {
        let t = ident(toks.get(i + 1))?;
        i += 2;
        (Some(first), t)
    } else {
        (None, first)
    };
    // Optional alias, with or without AS.
    if toks.get(i).is_some_and(|t| is_kw(t, "as")) {
        i += 1;
        ident(toks.get(i))?;
        i += 1;
    } else if ident(toks.get(i)).is_some() {
        i += 1;
    }
    match toks.get(i) {
        None | Some(Tok::Sym(';')) => Some((schema, table)),
        Some(Tok::Word(w)) if AFTER_TABLE.contains(&w.to_ascii_lowercase().as_str()) => {
            Some((schema, table))
        }
        // A comma here is an implicit join; anything else is unexpected.
        _ => None,
    }
}

/// `stmt` with `LIMIT n` added, when it's a plain SELECT (or WITH … SELECT)
/// that doesn't already limit itself; `None` leaves it alone. The limit
/// goes on its own line, so a trailing `-- comment` can't swallow it.
pub fn with_limit(stmt: &str, n: usize) -> Option<String> {
    let toks = top_level_tokens(stmt);
    let first = toks.iter().find(|t| matches!(t, Tok::Word(_)))?;
    let reads =
        is_kw(first, "select") || (is_kw(first, "with") && toks.iter().any(|t| is_kw(t, "select")));
    let limited = toks.iter().any(|t| {
        ["limit", "fetch", "offset", "top", "into", "for"]
            .iter()
            .any(|kw| is_kw(t, kw))
    });
    if !reads || limited {
        return None;
    }
    let body = stmt.trim_end().trim_end_matches(';').trim_end();
    Some(format!("{body}\nLIMIT {n}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(sql: &str) -> Option<(Option<String>, String)> {
        single_table(sql, Driver::Postgres)
    }

    fn named(name: &str) -> Option<(Option<String>, String)> {
        Some((None, name.into()))
    }

    #[test]
    fn single_table_selects_are_found() {
        assert_eq!(t("select * from users"), named("users"));
        assert_eq!(
            t("SELECT id, name FROM Users WHERE id > 3 ORDER BY id LIMIT 10"),
            named("users")
        );
        assert_eq!(t("select u.* from users u where u.id = 1"), named("users"));
        assert_eq!(t("select * from users as u;"), named("users"));
        assert_eq!(
            t(r#"select * from "Sales"."Orders""#),
            Some((Some("Sales".into()), "Orders".into()))
        );
        assert_eq!(
            t("select * from sales.orders o"),
            Some((Some("sales".into()), "orders".into()))
        );
        // Parens, strings and comments don't count.
        assert_eq!(
            t("select extract(year from created_at), (select 1 from x) from users"),
            named("users")
        );
        assert_eq!(
            t("select 'a join b' from users -- group by\n where id = 1"),
            named("users")
        );
        assert_eq!(
            single_table("select * from `Users`", Driver::MySql),
            named("Users")
        );
        assert_eq!(
            single_table("select * from Users", Driver::MySql),
            named("Users")
        );
    }

    #[test]
    fn anything_else_is_read_only() {
        for sql in [
            "select * from a join b on a.id = b.a_id",
            "select * from a, b",
            "select count(*) from users group by role",
            "select distinct role from users",
            "select * from a union select * from b",
            "with x as (select 1) select * from x",
            "select 1",
            "update users set a = 1",
            "select * into backup from users",
            "select * from generate_series(1, 3)",
            "select * from users natural join roles",
        ] {
            assert_eq!(t(sql), None, "{sql}");
        }
    }

    #[test]
    fn adds_a_limit_only_where_it_fits() {
        assert_eq!(
            with_limit("select * from t;", 100).as_deref(),
            Some("select * from t\nLIMIT 100")
        );
        assert_eq!(
            with_limit("SELECT a FROM t -- note", 5).as_deref(),
            Some("SELECT a FROM t -- note\nLIMIT 5")
        );
        assert!(with_limit("with x as (select 1) select * from x", 5).is_some());
        // A limit inside a subquery doesn't count.
        assert!(with_limit("select * from (select * from t limit 3) s", 5).is_some());
        for sql in [
            "select * from t limit 10",
            "select * from t offset 5",
            "select * from t fetch first 3 rows only",
            "select * from t for update",
            "select 1 into @x",
            "update t set a = 1",
            "with x as (select 1) delete from t",
            "insert into t select * from u",
        ] {
            assert_eq!(with_limit(sql, 5), None, "{sql}");
        }
    }
}
