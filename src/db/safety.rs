//! Production safe mode: decides whether editor text only reads data, so it
//! can run without a confirmation prompt. When unsure, it says "writes".

use super::split::statements;
use super::{ConnectionConfig, Driver, SafeMode};

/// What to do before running something.
#[derive(Debug, PartialEq)]
pub enum Gate {
    Run,
    Confirm,
    Block(String),
}

const READ_ONLY: &str =
    "This connection is read-only (Safe mode). Change it in the connection's settings.";

/// For editor text.
pub fn gate_query(cfg: &ConnectionConfig, text: &str) -> Gate {
    let reads = || is_read_only(cfg.driver, text);
    match cfg.safety() {
        SafeMode::ReadOnly if !reads() => Gate::Block(READ_ONLY.into()),
        SafeMode::ConfirmAll => Gate::Confirm,
        SafeMode::ConfirmWrites if !reads() => Gate::Confirm,
        _ => Gate::Run,
    }
}

/// For grid commits, imports, drop / truncate.
pub fn gate_write(cfg: &ConnectionConfig) -> Gate {
    match cfg.safety() {
        SafeMode::ReadOnly => Gate::Block(READ_ONLY.into()),
        SafeMode::ConfirmWrites | SafeMode::ConfirmAll => Gate::Confirm,
        _ => Gate::Run,
    }
}

pub fn is_read_only(driver: Driver, text: &str) -> bool {
    match driver {
        Driver::Redis => redis_read_only(text),
        Driver::Mongo => mongo_read_only(text),
        Driver::Mssql => {
            let stmts = statements(driver, text);
            !stmts.is_empty() && stmts.iter().all(|s| mssql_read_only(s))
        }
        _ => {
            let mysql = driver == Driver::MySql;
            let stmts = statements(driver, text);
            !stmts.is_empty() && stmts.iter().all(|s| sql_read_only(s, mysql))
        }
    }
}

/// Lowercase words of a statement with comments removed. Quoted text is
/// kept as words (so `'into'` still counts: when unsure, it "writes"), but
/// `--` or `/*` inside quotes doesn't start a comment. Quotes follow the
/// same escaping rules as `split_statements`.
fn words(stmt: &str, backslash_escapes: bool) -> Vec<String> {
    let chars: Vec<char> = stmt.chars().collect();
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '-' if next == Some('-') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                text.push(' ');
            }
            '/' if next == Some('*') => {
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    i += 1;
                }
                i += 2;
                text.push(' ');
            }
            '\'' | '"' | '`' => {
                let e_string = c == '\''
                    && i > 0
                    && matches!(chars[i - 1], 'e' | 'E')
                    && (i < 2 || !(chars[i - 2].is_alphanumeric() || chars[i - 2] == '_'));
                let escapes = c != '"' && (backslash_escapes || e_string);
                text.push(' ');
                i += 1;
                while i < chars.len() {
                    if chars[i] == c {
                        if chars.get(i + 1) == Some(&c) {
                            text.push(c);
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    if chars[i] == '\\' && escapes {
                        i += 1;
                    }
                    if let Some(&ch) = chars.get(i) {
                        text.push(ch);
                    }
                    i += 1;
                }
                i += 1;
                text.push(' ');
            }
            _ => {
                text.push(c);
                i += 1;
            }
        }
    }
    text.to_ascii_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

const DATA_CHANGING: &[&str] = &[
    "insert", "update", "delete", "merge", "truncate", "drop", "alter", "create",
];

fn sql_read_only(stmt: &str, backslash_escapes: bool) -> bool {
    let w = words(stmt, backslash_escapes);
    let has = |k: &str| w.iter().any(|x| x == k);
    let Some(first) = w.first() else { return true };
    match first.as_str() {
        // SELECT … INTO creates a table.
        "select" | "values" | "table" => !has("into"),
        "show" | "describe" | "desc" => true,
        // `WITH x AS (DELETE … RETURNING *) SELECT …` writes.
        "with" => !has("into") && !DATA_CHANGING.iter().any(|k| has(k)),
        // EXPLAIN ANALYZE runs the statement.
        "explain" => !has("analyze") && !has("analyse"),
        // PRAGMA name = value changes settings.
        "pragma" => !stmt.contains('='),
        _ => false,
    }
}

/// Words that make a T-SQL statement write, or do something else than
/// read: `EXEC` can run anything, `NEXT VALUE FOR` advances a sequence.
const MSSQL_NOT_READING: &[&str] = &[
    "into",
    "exec",
    "execute",
    "set",
    "use",
    "grant",
    "revoke",
    "deny",
    "backup",
    "restore",
    "dbcc",
    "kill",
    "shutdown",
    "reconfigure",
    "bulk",
    "openrowset",
    "opendatasource",
    "openquery",
    "next",
    "begin",
    "commit",
    "rollback",
    "save",
    "writetext",
    "updatetext",
];

/// T-SQL needs no `;` between statements, so `select 1 delete from t` is
/// one "statement": every word counts, not only the first.
fn mssql_read_only(stmt: &str) -> bool {
    let w = words(stmt, false);
    let Some(first) = w.first() else { return true };
    matches!(first.as_str(), "select" | "with")
        && !w.iter().any(|word| {
            DATA_CHANGING.contains(&word.as_str()) || MSSQL_NOT_READING.contains(&word.as_str())
        })
}

const REDIS_READS: &[&str] = &[
    "GET",
    "MGET",
    "STRLEN",
    "GETRANGE",
    "EXISTS",
    "TTL",
    "PTTL",
    "TYPE",
    "KEYS",
    "SCAN",
    "HGET",
    "HMGET",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HEXISTS",
    "HSCAN",
    "LRANGE",
    "LLEN",
    "LINDEX",
    "SMEMBERS",
    "SCARD",
    "SISMEMBER",
    "SSCAN",
    "ZRANGE",
    "ZRANGEBYSCORE",
    "ZSCORE",
    "ZCARD",
    "ZRANK",
    "ZCOUNT",
    "ZSCAN",
    "XRANGE",
    "XREVRANGE",
    "XLEN",
    "INFO",
    "DBSIZE",
    "PING",
    "ECHO",
    "TIME",
    "MEMORY",
    "OBJECT",
];

fn redis_read_only(text: &str) -> bool {
    let cmds: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            l.split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_uppercase()
        })
        .collect();
    !cmds.is_empty() && cmds.iter().all(|c| REDIS_READS.contains(&c.as_str()))
}

const MONGO_READS: &[&str] = &[
    "find",
    "findOne",
    "countDocuments",
    "count",
    "estimatedDocumentCount",
    "distinct",
];

/// The method of a `db.<coll>.<method>(…)` call, as `MongoConn::run` parses it.
fn mongo_method(text: &str) -> Option<&str> {
    let rest = text.trim().strip_prefix("db.")?;
    let rest = match rest.strip_prefix("getCollection(") {
        Some(r) => &r[r.find(')')? + 1..],
        None => &rest[rest.find('.')?..],
    };
    let rest = rest.strip_prefix('.')?;
    Some(rest[..rest.find('(')?].trim())
}

fn mongo_read_only(text: &str) -> bool {
    // Raw command documents can do anything.
    match mongo_method(text) {
        Some("aggregate") => !text.contains("$out") && !text.contains("$merge"),
        Some(m) => MONGO_READS.contains(&m),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ro(d: Driver, t: &str) -> bool {
        is_read_only(d, t)
    }

    #[test]
    fn every_sql_statement_must_read() {
        let pg = Driver::Postgres;
        assert!(ro(pg, "select * from users"));
        assert!(ro(
            pg,
            "-- report\nSELECT 1; show search_path; explain select 1"
        ));
        assert!(
            !ro(pg, "select 1; drop table users;"),
            "second statement writes"
        );
        assert!(!ro(
            pg,
            "with d as (delete from t returning *) select * from d"
        ));
        assert!(ro(pg, "with a as (select 1) select * from a"));
        assert!(!ro(pg, "EXPLAIN ANALYZE DELETE FROM t"));
        assert!(!ro(pg, "select * into backup from users"));
        assert!(!ro(pg, "/* select */ delete from t"));
        // Comment markers inside strings don't hide the rest of the statement.
        assert!(!ro(pg, "select '--' as a into backup from users"));
        assert!(!ro(pg, "with x as (select '/*') delete from t -- */"));
        assert!(!ro(
            Driver::MySql,
            r"select 'a\' -- ' into outfile '/tmp/x'"
        ));
        assert!(!ro(pg, "update t set a = 1"));
        assert!(!ro(pg, ""), "nothing to run → ask");
        assert!(ro(Driver::Sqlite, "pragma table_info(t)"));
        assert!(!ro(Driver::Sqlite, "pragma foreign_keys = off"));
        // MySQL escaping doesn't hide a second statement.
        assert!(!ro(
            Driver::MySql,
            r"select 'a\'; drop table t'; drop table u"
        ));
    }

    #[test]
    fn gates_follow_the_safe_mode() {
        use crate::db::EnvTag;
        let mut c = ConnectionConfig::new(Driver::Postgres);
        let (read, write) = ("select 1", "delete from t");
        assert_eq!(gate_query(&c, write), Gate::Run, "local + auto = off");
        c.tag = EnvTag::Production;
        assert_eq!(gate_query(&c, read), Gate::Run);
        assert_eq!(gate_query(&c, write), Gate::Confirm);
        assert_eq!(gate_write(&c), Gate::Confirm);
        c.safe_mode = SafeMode::Off;
        assert_eq!(
            gate_query(&c, write),
            Gate::Run,
            "explicit off wins over the tag"
        );
        c.safe_mode = SafeMode::ConfirmAll;
        assert_eq!(gate_query(&c, read), Gate::Confirm);
        c.safe_mode = SafeMode::ReadOnly;
        assert_eq!(gate_query(&c, read), Gate::Run);
        assert!(matches!(gate_query(&c, write), Gate::Block(_)));
        assert!(matches!(gate_write(&c), Gate::Block(_)));
    }

    #[test]
    fn tsql_reads_need_every_word_to_read() {
        let ms = Driver::Mssql;
        assert!(ro(
            ms,
            "select top 10 * from dbo.users where name like 'a%'"
        ));
        assert!(ro(
            ms,
            "with x as (select 1 as a) select a from x\nGO\nselect 2"
        ));
        assert!(!ro(ms, "select 1 delete from users"), "no `;` needed");
        assert!(!ro(ms, "select * into copy from users"));
        assert!(!ro(ms, "exec sp_who"));
        assert!(!ro(ms, "declare @n int = 1; select @n"));
        assert!(!ro(ms, "select next value for seq"));
    }

    #[test]
    fn redis_checks_every_line() {
        assert!(ro(Driver::Redis, "GET k\n# note\nhgetall user:1"));
        assert!(!ro(Driver::Redis, "GET k\nFLUSHALL"));
        assert!(!ro(Driver::Redis, "SET k v"));
    }

    #[test]
    fn mongo_reads_the_method_not_substrings() {
        let m = Driver::Mongo;
        assert!(ro(m, "db.users.find({a: 1}).sort({b: 1})"));
        assert!(ro(m, r#"db.getCollection("a.b").countDocuments({})"#));
        assert!(!ro(m, r#"db.x.deleteMany({a: ".count"})"#));
        assert!(!ro(m, r#"db.x.updateOne({}, {$set: {find: 1}})"#));
        assert!(ro(m, "db.x.aggregate([{$match: {}}])"));
        assert!(!ro(m, r#"db.x.aggregate([{$out: "y"}])"#));
        assert!(!ro(m, r#"{ "dropDatabase": 1 }"#));
    }
}
