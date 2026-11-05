//! SQL formatter (⌘⇧F): keywords in upper case, each clause on its own
//! line, select lists / conditions one item per line, subqueries indented.
//! Strings, quoted identifiers and comments are kept exactly as written.

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    /// String literal, quoted identifier, dollar-quoted body, number…:
    /// copied verbatim.
    Lit(String),
    LineComment(String),
    BlockComment(String),
    Open,
    Close,
    Comma,
    Semi,
    Dot,
    /// Any other operator / punctuation run, e.g. `=`, `<>`, `::`, `->>`.
    Op(String),
}

fn tokenize(sql: &str, backslash_escapes: bool) -> Vec<Tok> {
    let c: Vec<char> = sql.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    let take = |from: usize, to: usize| c[from..to.min(c.len())].iter().collect::<String>();
    while i < c.len() {
        let ch = c[i];
        let next = c.get(i + 1).copied();
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '-' && next == Some('-') {
            let s = i;
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            out.push(Tok::LineComment(take(s, i).trim_end().to_string()));
        } else if ch == '#' && backslash_escapes {
            // MySQL: `#` comments run to the end of the line.
            let s = i;
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            out.push(Tok::LineComment(take(s, i).trim_end().to_string()));
        } else if ch == '/' && next == Some('*') {
            let s = i;
            i += 2;
            while i < c.len() && !(c[i] == '*' && c.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i = (i + 2).min(c.len());
            out.push(Tok::BlockComment(take(s, i)));
        } else if matches!(ch, '\'' | '"' | '`')
            || (matches!(ch, 'E' | 'e' | 'N' | 'n' | 'X' | 'x' | 'B' | 'b') && next == Some('\''))
        {
            let s = i;
            if ch != '\'' && ch != '"' && ch != '`' {
                i += 1; // E'', N'', X'', B'' prefix: part of the literal
            }
            let q = c[i];
            let escapes = backslash_escapes || matches!(ch, 'E' | 'e');
            i += 1;
            while i < c.len() {
                if escapes && c[i] == '\\' {
                    i += 2;
                    continue;
                }
                if c[i] == q {
                    if c.get(i + 1) == Some(&q) {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i = (i + 1).min(c.len());
            out.push(Tok::Lit(take(s, i)));
        } else if ch == '[' {
            let s = i;
            while i < c.len() && c[i] != ']' {
                i += 1;
            }
            i = (i + 1).min(c.len());
            out.push(Tok::Lit(take(s, i)));
        } else if ch == '$' && next.is_some_and(|n| n == '$' || n.is_alphabetic() || n == '_') {
            // $tag$ … $tag$ (Postgres), else a parameter like $1 below.
            let s = i;
            let mut j = i + 1;
            while j < c.len() && (c[j].is_alphanumeric() || c[j] == '_') {
                j += 1;
            }
            if c.get(j) == Some(&'$') {
                let tag = take(s, j + 1);
                let body_start = j + 1;
                let rest: String = c[body_start..].iter().collect();
                let end = rest
                    .find(&tag)
                    .map(|e| body_start + rest[..e].chars().count() + tag.chars().count())
                    .unwrap_or(c.len());
                out.push(Tok::Lit(take(s, end)));
                i = end;
            } else {
                out.push(Tok::Lit(take(s, j)));
                i = j;
            }
        } else if ch.is_alphanumeric() || ch == '_' || ch == '@' || ch == '#' || ch == '$' || ch == ':' && next.is_some_and(|n| n.is_alphabetic()) {
            let s = i;
            i += 1;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '$') {
                i += 1;
            }
            // Decimals like 1.5 and exponents stay one token.
            if ch.is_ascii_digit() {
                loop {
                    // Signed exponent: 1e-5, 2.5E+10.
                    let signed_exp = matches!(c[i - 1], 'e' | 'E')
                        && matches!(c.get(i), Some('+' | '-'))
                        && c.get(i + 1).is_some_and(char::is_ascii_digit);
                    if signed_exp || (i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '.')) {
                        i += 1;
                    } else {
                        break;
                    }
                }
                out.push(Tok::Lit(take(s, i)));
            } else {
                out.push(Tok::Word(take(s, i)));
            }
        } else {
            i += 1;
            out.push(match ch {
                '(' => Tok::Open,
                ')' => Tok::Close,
                ',' => Tok::Comma,
                ';' => Tok::Semi,
                '.' => Tok::Dot,
                _ => {
                    let s = i - 1;
                    while i < c.len()
                        && "=<>!:+-*/%|&^~?".contains(c[i])
                        && !(c[i] == '-' && c.get(i + 1) == Some(&'-'))
                        && !(c[i] == '/' && c.get(i + 1) == Some(&'*'))
                    {
                        i += 1;
                    }
                    Tok::Op(take(s, i))
                }
            });
        }
    }
    out
}

/// Words written in upper case. Anything else (identifiers, functions not
/// listed) keeps the user's spelling.
const KEYWORDS: &[&str] = &[
    "select", "from", "where", "and", "or", "not", "in", "is", "null", "as", "on", "join",
    "inner", "left", "right", "full", "outer", "cross", "natural", "using", "group", "by",
    "order", "having", "limit", "offset", "fetch", "first", "next", "rows", "only", "union",
    "all", "intersect", "except", "distinct", "insert", "into", "values", "update", "set",
    "delete", "returning", "with", "recursive", "case", "when", "then", "else", "end", "asc",
    "desc", "nulls", "last", "between", "like", "ilike", "exists", "any", "some", "create",
    "table", "view", "index", "unique", "primary", "key", "foreign", "references", "default",
    "constraint", "check", "alter", "add", "column", "drop", "truncate", "if", "cascade",
    "begin", "commit", "rollback", "transaction", "true", "false", "window", "over",
    "partition", "lateral", "conflict", "do", "nothing", "for", "share", "explain", "analyze",
    "count", "sum", "avg", "min", "max", "coalesce", "cast", "interval", "current_date",
    "current_timestamp", "now", "lower", "upper", "replace", "ignore", "duplicate",
];

/// Keywords that are also functions: their parens hug.
const FUNCS: &[&str] = &[
    "count", "sum", "avg", "min", "max", "coalesce", "cast", "now", "lower", "upper", "replace",
    "over", "exists",
];

/// Clauses that start a new line at the current statement's indent.
fn clause(words: &[String], i: usize) -> Option<usize> {
    let w = |k: usize| words.get(i + k).map(String::as_str).unwrap_or("");
    let two = |a: &str, b: &str| w(0) == a && w(1) == b;
    Some(match w(0) {
        "select" | "from" | "where" | "having" | "limit" | "offset" | "values" | "set"
        | "returning" | "window" | "fetch" | "using" => 1,
        "union" | "intersect" | "except" => {
            if w(1) == "all" || w(1) == "distinct" {
                2
            } else {
                1
            }
        }
        "group" | "order" | "partition" if w(1) == "by" => 2,
        "insert" if w(1) == "into" => 2,
        "delete" if w(1) == "from" => 2,
        "update" => 1,
        "on" if two("on", "conflict") => 2,
        "join" => 1,
        "inner" | "cross" | "natural" if w(1) == "join" => 2,
        "left" | "right" | "full" => {
            if w(1) == "outer" && w(2) == "join" {
                3
            } else if w(1) == "join" {
                2
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

struct Out {
    text: String,
    indent: usize,
    /// Nothing written yet on the current line.
    fresh: bool,
}

impl Out {
    fn newline(&mut self, indent: usize) {
        while self.text.ends_with(' ') {
            self.text.pop();
        }
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        self.indent = indent;
        self.fresh = true;
    }

    fn word(&mut self, s: &str, space_before: bool) {
        if self.fresh {
            self.text.push_str(&"  ".repeat(self.indent));
            self.fresh = false;
        } else if space_before && !self.text.ends_with([' ', '(', '.']) {
            self.text.push(' ');
        }
        self.text.push_str(s);
    }
}

/// Formats `sql` (one or more statements). Never changes the meaning: only
/// whitespace and the case of known keywords change.
pub fn format_sql(sql: &str, backslash_escapes: bool) -> String {
    let toks = tokenize(sql, backslash_escapes);
    let lower: Vec<String> = toks
        .iter()
        .map(|t| match t {
            Tok::Word(w) => w.to_ascii_lowercase(),
            _ => String::new(),
        })
        .collect();
    let mut out = Out {
        text: String::new(),
        indent: 0,
        fresh: true,
    };
    // Per open paren: (base indent inside it, is a subquery, indent of
    // the line that opened it — where the `)` goes).
    let mut stack: Vec<(usize, bool, usize)> = vec![];
    let base =
        |stack: &Vec<(usize, bool, usize)>| stack.iter().rev().find(|s| s.1).map_or(0, |s| s.0);
    // The clause we're in decides how commas and AND/OR break.
    let mut in_list = false; // SELECT list / SET / ORDER BY / GROUP BY items
    let mut in_cond = false; // WHERE / HAVING / ON conditions
    let mut between = false;
    let mut case_depth = 0usize;
    let mut i = 0;
    while i < toks.len() {
        let depth_base = base(&stack);
        // Inside plain parens (calls, IN lists) nothing breaks.
        let plain = stack.last().is_some_and(|s| !s.1);
        match &toks[i] {
            Tok::Word(w) => {
                let lw = &lower[i];
                if !plain && case_depth == 0 {
                    if let Some(n) = clause(&lower, i) {
                        out.newline(depth_base);
                        for k in 0..n {
                            out.word(&lower[i + k].to_ascii_uppercase(), true);
                        }
                        let head = lower[i].as_str();
                        in_list = matches!(head, "select" | "set" | "order" | "group" | "partition" | "returning");
                        in_cond = matches!(head, "where" | "having");
                        // Long lists start on the next line, indented.
                        if in_list || in_cond {
                            out.newline(depth_base + 1);
                        }
                        i += n;
                        continue;
                    }
                    if lw == "on" && !in_cond {
                        out.newline(depth_base + 1);
                        out.word("ON", true);
                        in_cond = true;
                        in_list = false;
                        i += 1;
                        continue;
                    }
                    if in_cond && (lw == "and" || lw == "or") && !between {
                        out.newline(depth_base + 1);
                        out.word(&lw.to_ascii_uppercase(), true);
                        i += 1;
                        continue;
                    }
                }
                if lw == "between" {
                    between = true;
                } else if lw == "and" && between {
                    between = false;
                }
                if lw == "case" {
                    case_depth += 1;
                } else if lw == "end" && case_depth > 0 {
                    case_depth -= 1;
                }
                let after_dot = i > 0 && toks[i - 1] == Tok::Dot;
                let word = if KEYWORDS.contains(&lw.as_str()) && !after_dot {
                    lw.to_ascii_uppercase()
                } else {
                    w.clone()
                };
                out.word(&word, !after_dot);
            }
            Tok::Lit(s) => {
                let after_dot = i > 0 && toks[i - 1] == Tok::Dot;
                out.word(s, !after_dot);
            }
            Tok::LineComment(s) => {
                out.word(s, true);
                out.newline(out.indent);
            }
            Tok::BlockComment(s) => out.word(s, true),
            Tok::Open => {
                let sub = lower.get(i + 1).is_some_and(|w| w == "select" || w == "with");
                // A call's paren hugs its name: `count(*)`, `my_fn(x)`; but
                // not `IN (…)`, `VALUES (…)`, `INSERT INTO t (…)`.
                let prev = if i > 0 { lower[i - 1].as_str() } else { "" };
                let before = if i > 1 { lower[i - 2].as_str() } else { "" };
                let hug = !sub
                    && i > 0
                    && matches!(toks[i - 1], Tok::Word(_))
                    && (FUNCS.contains(&prev)
                        || (!KEYWORDS.contains(&prev)
                            && !matches!(before, "into" | "table" | "index" | "view" | "references")));
                if hug {
                    out.text.push('(');
                    out.fresh = false;
                } else {
                    out.word("(", true);
                }
                if sub {
                    stack.push((out.indent + 1, true, out.indent));
                } else {
                    stack.push((0, false, 0));
                }
            }
            Tok::Close => {
                let was = stack.pop();
                if let Some((_, true, close_at)) = was {
                    out.newline(close_at);
                    out.word(")", false);
                    // Back in the outer clause.
                    in_list = false;
                    in_cond = false;
                } else {
                    while out.text.ends_with(' ') {
                        out.text.pop();
                    }
                    out.text.push(')');
                    out.fresh = false;
                }
            }
            Tok::Comma => {
                while out.text.ends_with(' ') {
                    out.text.pop();
                }
                out.text.push(',');
                out.fresh = false;
                if in_list && !plain && case_depth == 0 {
                    out.newline(depth_base + 1);
                }
            }
            Tok::Semi => {
                while out.text.ends_with(' ') {
                    out.text.pop();
                }
                out.text.push(';');
                out.text.push('\n');
                out.fresh = true;
                out.indent = 0;
                stack.clear();
                in_list = false;
                in_cond = false;
                case_depth = 0;
            }
            Tok::Dot => {
                while out.text.ends_with(' ') {
                    out.text.pop();
                }
                out.text.push('.');
                out.fresh = false;
            }
            Tok::Op(o) => {
                // `::type` casts hug both sides.
                if o == "::" {
                    out.text.push_str("::");
                    out.fresh = false;
                    i += 1;
                    if let Some(Tok::Word(w)) = toks.get(i) {
                        out.text.push_str(w);
                        i += 1;
                    }
                    continue;
                }
                out.word(o, true);
            }
        }
        i += 1;
    }
    let mut s = out.text.trim_end().to_string();
    s.push('\n');
    // A trailing newline only when the input had one.
    if !sql.ends_with('\n') {
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(s: &str) -> String {
        format_sql(s, false)
    }

    #[test]
    fn breaks_clauses_and_lists() {
        assert_eq!(
            f("select id, name, count(*) as n from users u left join orders o on o.user_id = u.id and o.paid where u.active = true and u.age between 18 and 65 group by id, name order by n desc limit 10"),
            "SELECT\n  id,\n  name,\n  COUNT(*) AS n\nFROM users u\nLEFT JOIN orders o\n  ON o.user_id = u.id\n  AND o.paid\nWHERE\n  u.active = TRUE\n  AND u.age BETWEEN 18 AND 65\nGROUP BY\n  id,\n  name\nORDER BY\n  n DESC\nLIMIT 10"
        );
    }

    #[test]
    fn indents_subqueries_and_keeps_literals() {
        assert_eq!(
            f("select * from (select id from t where note = 'select, from' ) x where id in (1,2)"),
            "SELECT\n  *\nFROM (\n  SELECT\n    id\n  FROM t\n  WHERE\n    note = 'select, from'\n) x\nWHERE\n  id IN (1, 2)"
        );
    }

    #[test]
    fn keeps_meaningful_tokens_intact() {
        let out = f("SELECT \"Order\".\"Select\", data->>'k', x::int, $1, $$a;b$$ FROM \"Order\" -- note\n;");
        assert!(out.contains("\"Order\".\"Select\""), "{out}");
        assert!(out.contains("data ->> 'k'"), "{out}");
        assert!(out.contains("x::int"), "{out}");
        assert!(out.contains("$1"), "{out}");
        assert!(out.contains("$$a;b$$"), "{out}");
        assert!(out.contains("-- note"), "{out}");
    }

    #[test]
    fn keeps_exponents_prefixed_literals_and_mysql_comments() {
        let out = f("select 1e-5, 2.5E+10, x'4D', n'abc' from t");
        assert!(out.contains("1e-5,"), "{out}");
        assert!(out.contains("2.5E+10"), "{out}");
        assert!(out.contains("x'4D'"), "{out}");
        assert!(out.contains("n'abc'"), "{out}");
        let my = format_sql("select a # from here\n, b from t", true);
        assert!(my.contains("# from here\n"), "{my}");
        assert_eq!(my.matches("FROM").count(), 1, "{my}");
    }

    #[test]
    fn formatting_is_stable_and_meaning_preserving() {
        let samples = [
            "select a, b from t where x = 1 or y = 2 order by a",
            "insert into t (a, b) values (1, 'x'), (2, 'y'); update t set a = 2, b = 'z' where id = 1; delete from t where id = 3",
            "with x as (select 1 as n) select n from x union all select 2",
            "select case when a > 1 then 'big' else 'small' end as size from t",
        ];
        for s in samples {
            let once = f(s);
            assert_eq!(f(&once), once, "idempotent for {s}");
            // Same tokens, ignoring case and whitespace.
            let norm = |x: &str| {
                tokenize(x, false)
                    .into_iter()
                    .map(|t| match t {
                        Tok::Word(w) => Tok::Word(w.to_ascii_lowercase()),
                        t => t,
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(norm(&once), norm(s), "tokens kept for {s}");
        }
    }
}
