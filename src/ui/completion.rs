//! Editor autocomplete: schema-aware suggestions for SQL, collection/method
//! names for MongoDB and command names for Redis.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use anyhow::Result;
use gpui::{App, Task, Window};
use gpui_component::input::{CompletionProvider, Rope, RopeExt as _};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Range, TextEdit,
};

use crate::db::Driver;

/// What the editor knows about the database: object names and their columns.
#[derive(Default)]
pub struct Schema {
    pub tables: Vec<String>,
    /// lowercase table name → columns
    pub columns: BTreeMap<String, Vec<String>>,
}

impl Schema {
    pub fn new(tables: Vec<String>, cols: Vec<(String, String)>) -> Self {
        let mut columns: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (t, c) in cols {
            columns.entry(t.to_lowercase()).or_default().push(c);
        }
        Self { tables, columns }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Column,
    Table,
    Keyword,
    Function,
    Method,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub label: String,
    pub kind: Kind,
    pub detail: Option<String>,
}

const SQL_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS",
    "NULL",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "EXISTS",
    "JOIN",
    "INNER JOIN",
    "LEFT JOIN",
    "RIGHT JOIN",
    "FULL JOIN",
    "CROSS JOIN",
    "ON",
    "USING",
    "AS",
    "DISTINCT",
    "GROUP BY",
    "ORDER BY",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "ASC",
    "DESC",
    "UNION",
    "UNION ALL",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE FROM",
    "RETURNING",
    "CREATE TABLE",
    "CREATE INDEX",
    "CREATE VIEW",
    "ALTER TABLE",
    "ADD COLUMN",
    "DROP TABLE",
    "DROP INDEX",
    "TRUNCATE",
    "PRIMARY KEY",
    "FOREIGN KEY",
    "REFERENCES",
    "DEFAULT",
    "UNIQUE",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "WITH",
    "EXPLAIN",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "TRUE",
    "FALSE",
    "CAST",
    "INTERVAL",
    "OVER",
    "PARTITION BY",
];

const SQL_FUNCTIONS: &[&str] = &[
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "COALESCE",
    "NULLIF",
    "LOWER",
    "UPPER",
    "TRIM",
    "LENGTH",
    "SUBSTRING",
    "REPLACE",
    "CONCAT",
    "ROUND",
    "ABS",
    "NOW",
    "CURRENT_DATE",
    "CURRENT_TIMESTAMP",
    "DATE",
    "EXTRACT",
    "ROW_NUMBER",
    "RANK",
    "DENSE_RANK",
    "LAG",
    "LEAD",
    "STRING_AGG",
    "ARRAY_AGG",
    "JSON_AGG",
    "DATE_TRUNC",
    "GROUP_CONCAT",
    "IFNULL",
    "STRFTIME",
];

/// Keywords after which the next word is a table name.
const TABLE_CONTEXT: &[&str] = &[
    "from", "join", "into", "update", "table", "describe", "desc",
];

const MONGO_METHODS: &[&str] = &[
    "find",
    "findOne",
    "aggregate",
    "countDocuments",
    "insertOne",
    "insertMany",
    "updateOne",
    "updateMany",
    "deleteOne",
    "deleteMany",
    "drop",
];

const REDIS_COMMANDS: &[&str] = &[
    "GET",
    "SET",
    "DEL",
    "EXISTS",
    "EXPIRE",
    "TTL",
    "PERSIST",
    "TYPE",
    "KEYS",
    "SCAN",
    "RENAME",
    "INCR",
    "DECR",
    "MGET",
    "MSET",
    "HGET",
    "HSET",
    "HGETALL",
    "HDEL",
    "HKEYS",
    "HVALS",
    "LPUSH",
    "RPUSH",
    "LPOP",
    "RPOP",
    "LRANGE",
    "LLEN",
    "SADD",
    "SREM",
    "SMEMBERS",
    "SCARD",
    "ZADD",
    "ZREM",
    "ZRANGE",
    "ZSCORE",
    "ZCARD",
    "XADD",
    "XRANGE",
    "XLEN",
    "INFO",
    "DBSIZE",
    "PING",
    "SELECT",
    "FLUSHDB",
    "CONFIG GET",
];

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Byte offset where the identifier ending at `offset` starts.
fn word_start(text: &str, offset: usize) -> usize {
    text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_ident(*c))
        .last()
        .map_or(offset, |(i, _)| i)
}

/// The identifier right before a `.` that precedes `start`, unquoted.
fn qualifier(text: &str, start: usize) -> Option<String> {
    let before = text[..start].strip_suffix('.')?;
    let before = before.trim_end_matches(['"', '`', ']']);
    let s = word_start(before, before.len());
    let q = &before[s..];
    (!q.is_empty()).then(|| q.to_string())
}

/// Splits into lowercase-preserving word tokens (identifiers and keywords).
fn tokens(text: &str) -> Vec<&str> {
    text.split(|c: char| !(is_ident(c) || c == '.'))
        .filter(|t| !t.is_empty())
        .collect()
}

/// The statement around `offset` (text between the surrounding semicolons).
fn statement(text: &str, offset: usize) -> (&str, usize) {
    let s = text[..offset].rfind(';').map_or(0, |i| i + 1);
    let e = text[offset..].find(';').map_or(text.len(), |i| offset + i);
    (&text[s..e], s)
}

/// alias/table name (lowercase) → real table name, from FROM/JOIN clauses.
fn aliases(stmt: &str, schema: &Schema) -> BTreeMap<String, String> {
    let toks = tokens(stmt);
    let mut out = BTreeMap::new();
    for (i, t) in toks.iter().enumerate() {
        if !matches!(
            t.to_lowercase().as_str(),
            "from" | "join" | "update" | "into"
        ) {
            continue;
        }
        let Some(name) = toks.get(i + 1) else {
            continue;
        };
        let table = name.rsplit('.').next().unwrap_or(name).to_string();
        out.insert(table.to_lowercase(), table.clone());
        let mut j = i + 2;
        if toks.get(j).is_some_and(|a| a.eq_ignore_ascii_case("as")) {
            j += 1;
        }
        if let Some(alias) = toks.get(j) {
            let reserved = SQL_KEYWORDS
                .iter()
                .any(|k| k.split(' ').next().unwrap().eq_ignore_ascii_case(alias))
                || schema.columns.contains_key(&alias.to_lowercase());
            if !reserved {
                out.insert(alias.to_lowercase(), table);
            }
        }
    }
    out
}

/// Exact match first, then prefix matches, then substring matches;
/// case-insensitive.
fn rank(prefix: &str, mut items: Vec<Suggestion>, limit: usize) -> Vec<Suggestion> {
    let p = prefix.to_lowercase();
    let mut seen = std::collections::HashSet::new();
    items.retain(|s| seen.insert(s.label.to_lowercase()));
    let score = |s: &Suggestion| {
        let l = s.label.to_lowercase();
        if l == p {
            Some(0)
        } else if l.starts_with(&p) {
            Some(1)
        } else if p.len() >= 2 && l.contains(&p) {
            Some(2)
        } else {
            None
        }
    };
    let mut scored: Vec<(u8, usize, Suggestion)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(i, s)| score(&s).map(|sc| (sc, i, s)))
        .collect();
    scored.sort_by_key(|(sc, i, _)| (*sc, *i));
    scored.into_iter().take(limit).map(|(_, _, s)| s).collect()
}

fn sugg(label: impl Into<String>, kind: Kind, detail: Option<String>) -> Suggestion {
    Suggestion {
        label: label.into(),
        kind,
        detail,
    }
}

/// Returns (byte offset where the replaced word starts, suggestions).
pub fn suggest(
    text: &str,
    offset: usize,
    schema: &Schema,
    driver: Driver,
) -> (usize, Vec<Suggestion>) {
    let offset = offset.min(text.len());
    let start = word_start(text, offset);
    let prefix = &text[start..offset];
    let items = match driver {
        Driver::Redis => redis(text, start),
        Driver::Mongo => mongo(text, start, schema),
        _ => sql(text, offset, start, schema),
    };
    (start, rank(prefix, items, 60))
}

fn sql(text: &str, offset: usize, start: usize, schema: &Schema) -> Vec<Suggestion> {
    let (stmt, stmt_start) = statement(text, offset);
    let aliases = aliases(stmt, schema);
    let cols_of = |table: &str| -> Vec<Suggestion> {
        schema
            .columns
            .get(&table.to_lowercase())
            .map(|cols| {
                cols.iter()
                    .map(|c| sugg(c, Kind::Column, Some(table.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    };

    // `alias.` / `table.` → that table's columns.
    if let Some(q) = qualifier(text, start) {
        let table = aliases.get(&q.to_lowercase()).cloned().unwrap_or(q.clone());
        let cols = cols_of(&table);
        if !cols.is_empty() {
            return cols;
        }
        // `schema.` → tables
        return schema
            .tables
            .iter()
            .map(|t| sugg(t, Kind::Table, None))
            .collect();
    }

    let before = tokens(&text[stmt_start..start]);
    let prev = before.last().map(|t| t.to_lowercase()).unwrap_or_default();
    let tables: Vec<Suggestion> = schema
        .tables
        .iter()
        .map(|t| sugg(t, Kind::Table, None))
        .collect();
    if TABLE_CONTEXT.contains(&prev.as_str()) {
        return tables;
    }

    // Columns of the tables this statement uses come first.
    let mut out: Vec<Suggestion> = aliases
        .values()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .flat_map(|t| cols_of(t))
        .collect();
    out.extend(SQL_KEYWORDS.iter().map(|k| sugg(*k, Kind::Keyword, None)));
    out.extend(SQL_FUNCTIONS.iter().map(|f| sugg(*f, Kind::Function, None)));
    out.extend(tables);
    out
}

fn mongo(text: &str, start: usize, schema: &Schema) -> Vec<Suggestion> {
    match qualifier(text, start).as_deref() {
        Some("db") => schema
            .tables
            .iter()
            .map(|t| sugg(t, Kind::Table, Some("collection".into())))
            .collect(),
        Some(_) => MONGO_METHODS
            .iter()
            .map(|m| sugg(*m, Kind::Method, None))
            .collect(),
        None => vec![sugg("db", Kind::Keyword, None)],
    }
}

fn redis(text: &str, start: usize) -> Vec<Suggestion> {
    // Only the first word on a line is a command.
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    if !text[line_start..start].trim().is_empty() {
        return vec![];
    }
    REDIS_COMMANDS
        .iter()
        .map(|c| sugg(*c, Kind::Keyword, None))
        .collect()
}

/// Plugs `suggest` into the editor. Reads the schema at request time, so it
/// sees reloads and database switches.
pub struct Provider {
    pub schema: Rc<RefCell<Schema>>,
    pub driver: Driver,
}

impl CompletionProvider for Provider {
    fn completions(
        &self,
        rope: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let text = rope.to_string();
        let (start, items) = suggest(&text, offset, &self.schema.borrow(), self.driver);
        let range = Range {
            start: rope.offset_to_position(start),
            end: rope.offset_to_position(offset),
        };
        let items = items
            .into_iter()
            .enumerate()
            .map(|(i, s)| CompletionItem {
                label: s.label.clone(),
                kind: Some(match s.kind {
                    Kind::Column => CompletionItemKind::FIELD,
                    Kind::Table => CompletionItemKind::STRUCT,
                    Kind::Keyword => CompletionItemKind::KEYWORD,
                    Kind::Function => CompletionItemKind::FUNCTION,
                    Kind::Method => CompletionItemKind::METHOD,
                }),
                detail: s.detail,
                sort_text: Some(format!("{i:04}")),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range,
                    new_text: s.label,
                })),
                ..Default::default()
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    /// Typing a single identifier character or `.` opens/refines the menu.
    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        let mut chars = new_text.chars();
        matches!((chars.next(), chars.next()), (Some(c), None) if is_ident(c) || c == '.')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Schema {
        Schema::new(
            vec!["users".into(), "orders".into(), "order_items".into()],
            vec![
                ("users".into(), "id".into()),
                ("users".into(), "email".into()),
                ("users".into(), "created_at".into()),
                ("orders".into(), "id".into()),
                ("orders".into(), "user_id".into()),
                ("orders".into(), "total".into()),
            ],
        )
    }

    fn labels(text: &str, driver: Driver) -> Vec<String> {
        let at = text.find('|').expect("cursor marker");
        let t = text.replace('|', "");
        suggest(&t, at, &schema(), driver)
            .1
            .into_iter()
            .map(|s| s.label)
            .collect()
    }

    #[test]
    fn tables_after_from_and_join() {
        assert_eq!(
            labels("select * from or|", Driver::Postgres),
            ["orders", "order_items"]
        );
        assert_eq!(
            labels("SELECT * FROM users u JOIN |", Driver::Postgres)[0],
            "users"
        );
    }

    #[test]
    fn alias_dot_gives_that_tables_columns() {
        let l = labels(
            "select o.| from orders o join users u on u.id = o.user_id",
            Driver::Postgres,
        );
        assert_eq!(l, ["id", "user_id", "total"]);
        assert_eq!(
            labels("select u.em| from users as u", Driver::Postgres),
            ["email"]
        );
        assert_eq!(
            labels("select users.cr| from users", Driver::MySql),
            ["created_at"]
        );
    }

    #[test]
    fn columns_of_statement_tables_then_keywords_matching_case() {
        let l = labels("select to| from orders", Driver::Postgres);
        assert_eq!(l[0], "total");
        let l = labels("sel|", Driver::Sqlite);
        assert_eq!(l, ["SELECT"], "built-ins are always uppercase");
        let l = labels("SELECT * FROM users WH|", Driver::Postgres);
        assert_eq!(l, ["WHERE", "WHEN"]);
        // Columns from another statement don't leak in.
        let l = labels(
            "select total from orders; select to| from users",
            Driver::Postgres,
        );
        assert!(!l.contains(&"total".to_string()), "{l:?}");
    }

    #[test]
    fn substring_matches_rank_after_prefix_matches() {
        let l = labels("select * from users where us|", Driver::Postgres);
        assert!(l.contains(&"users".to_string()));
        let l = labels("select * from orders where id|", Driver::Postgres);
        assert_eq!(l[0], "id", "exact match stays, ranked first");
        assert!(l.contains(&"user_id".to_string()));
        // Typing a whole keyword still offers it, so Enter capitalizes it.
        assert_eq!(labels("select|", Driver::Postgres)[0], "SELECT");
        // Exact match beats an earlier-listed prefix match.
        assert_eq!(
            labels("select * from order|", Driver::Postgres)[..2],
            ["orders", "order_items"]
        );
        assert_eq!(
            labels("select * from order_items|", Driver::Postgres)[0],
            "order_items"
        );
    }

    #[test]
    fn mongo_and_redis() {
        assert_eq!(labels("db.us|", Driver::Mongo), ["users"]);
        assert!(
            labels("db.users.fi|", Driver::Mongo)
                .starts_with(&["find".to_string(), "findOne".to_string()])
        );
        assert!(labels("hg|", Driver::Redis).contains(&"HGET".to_string()));
        assert!(labels("GET hg|", Driver::Redis).is_empty());
    }

    #[test]
    fn word_start_handles_unicode_and_edges() {
        assert_eq!(word_start("select é|", 9), 7);
        assert_eq!(word_start("", 0), 0);
        let (start, _) = suggest("select * from us", 16, &schema(), Driver::Postgres);
        assert_eq!(start, 14);
    }
}
