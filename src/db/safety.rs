//! Production safe mode: decides whether editor text only reads data, so it
//! can run without a confirmation prompt. When unsure, it says "writes".

use super::Driver;
use super::split::split_statements;

pub fn is_read_only(driver: Driver, text: &str) -> bool {
    match driver {
        Driver::Redis => redis_read_only(text),
        Driver::Mongo => mongo_read_only(text),
        _ => {
            let stmts = split_statements(text, driver == Driver::MySql);
            !stmts.is_empty() && stmts.iter().all(|s| sql_read_only(s))
        }
    }
}

/// Lowercase words of a statement with comments removed.
fn words(stmt: &str) -> Vec<String> {
    let mut text = String::new();
    let mut rest = stmt;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("--") {
            rest = r.split_once('\n').map_or("", |(_, r)| r);
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map_or("", |(_, r)| r);
        } else {
            let c = rest.chars().next().unwrap();
            text.push(c);
            rest = &rest[c.len_utf8()..];
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

fn sql_read_only(stmt: &str) -> bool {
    let w = words(stmt);
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
