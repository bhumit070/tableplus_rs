//! Redis: keys are shown as one "keys" table with key / type / ttl / value
//! columns. The editor runs one command per line.

use std::time::Instant;

use anyhow::{Result, bail};
use redis::Value;
use redis::aio::MultiplexedConnection;

use super::{
    Change, ColumnInfo, ConnectionConfig, DbObject, FilterOp, ObjectKind, QueryResult, Structure,
    TableQuery,
};

/// SCAN stops after this many keys so huge keyspaces stay responsive.
const SCAN_CAP: usize = 10_000;

pub struct RedisConn {
    cfg: ConnectionConfig,
    con: MultiplexedConnection,
    db: String,
}

fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

async fn open(cfg: &ConnectionConfig, db: &str) -> Result<MultiplexedConnection> {
    let scheme = if cfg.ssl { "rediss" } else { "redis" };
    let auth = match (cfg.user.is_empty(), cfg.password.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!(":{}@", pct(&cfg.password)),
        _ => format!("{}:{}@", pct(&cfg.user), pct(&cfg.password)),
    };
    let db = if db.is_empty() { "0" } else { db };
    let url = format!("{scheme}://{auth}{}:{}/{db}", cfg.host, cfg.port);
    // RESP3 returns hashes as maps; servers older than Redis 6 only speak RESP2.
    let resp3 = redis::Client::open(format!("{url}?protocol=resp3"))?;
    match resp3.get_multiplexed_async_connection().await {
        Ok(con) => Ok(con),
        Err(_) => Ok(redis::Client::open(url)?
            .get_multiplexed_async_connection()
            .await?),
    }
}

pub fn render(v: &Value) -> Option<String> {
    Some(match v {
        Value::Nil => return None,
        Value::Int(n) => n.to_string(),
        Value::BulkString(b) => String::from_utf8_lossy(b).into_owned(),
        Value::SimpleString(s) => s.clone(),
        Value::Okay => "OK".into(),
        Value::Double(d) => d.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Array(items) | Value::Set(items) => {
            let parts: Vec<serde_json::Value> = items
                .iter()
                .map(|i| {
                    render(i)
                        .map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null)
                })
                .collect();
            serde_json::Value::Array(parts).to_string()
        }
        Value::Map(pairs) => {
            let obj: serde_json::Map<String, serde_json::Value> = pairs
                .iter()
                .map(|(k, v)| {
                    (
                        render(k).unwrap_or_default(),
                        render(v)
                            .map(serde_json::Value::String)
                            .unwrap_or(serde_json::Value::Null),
                    )
                })
                .collect();
            serde_json::Value::Object(obj).to_string()
        }
        other => format!("{other:?}"),
    })
}

/// Splits a command line into words, honoring "double" and 'single' quotes.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(match n {
                        'n' => '\n',
                        't' => '\t',
                        o => o,
                    });
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn glob_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '*' | '?' | '[' | ']' | '\\' => vec!['\\', c],
            c => vec![c],
        })
        .collect()
}

impl RedisConn {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let db = if cfg.database.is_empty() {
            "0".to_string()
        } else {
            cfg.database.clone()
        };
        let mut con = open(cfg, &db).await?;
        redis::cmd("PING").query_async::<String>(&mut con).await?;
        Ok(Self {
            cfg: cfg.clone(),
            con,
            db,
        })
    }

    pub fn current_database(&self) -> String {
        self.db.clone()
    }

    /// CLIENT LIST as rows (our own client left out), first column = id.
    pub async fn processes(&self) -> Result<QueryResult> {
        let mut con = self.con.clone();
        let me: i64 = redis::cmd("CLIENT").arg("ID").query_async(&mut con).await?;
        let list: String = redis::cmd("CLIENT").arg("LIST").query_async(&mut con).await?;
        let cols = ["id", "addr", "name", "db", "age", "idle", "cmd"];
        let rows = list
            .lines()
            .filter_map(|line| {
                let kv: std::collections::HashMap<&str, &str> =
                    line.split(' ').filter_map(|p| p.split_once('=')).collect();
                (kv.get("id")? != &me.to_string().as_str()).then(|| {
                    cols.iter()
                        .map(|c| kv.get(c).map(|v| v.to_string()))
                        .collect()
                })
            })
            .collect();
        Ok(QueryResult {
            columns: cols.iter().map(|c| c.to_string()).collect(),
            rows,
            ..Default::default()
        })
    }

    /// CLIENT KILL ID: disconnects the client (Redis can't stop just a command).
    pub async fn kill(&self, id: &str) -> Result<()> {
        let mut con = self.con.clone();
        let n: i64 = redis::cmd("CLIENT")
            .arg("KILL")
            .arg("ID")
            .arg(id)
            .query_async(&mut con)
            .await?;
        if n == 0 {
            anyhow::bail!("no such client");
        }
        Ok(())
    }

    pub async fn databases(&self) -> Result<Vec<String>> {
        let mut con = self.con.clone();
        let n = redis::cmd("CONFIG")
            .arg("GET")
            .arg("databases")
            .query_async::<Vec<String>>(&mut con)
            .await
            .ok()
            .and_then(|v| v.get(1)?.parse::<usize>().ok())
            .unwrap_or(16);
        Ok((0..n).map(|i| i.to_string()).collect())
    }

    pub async fn use_database(&mut self, name: &str) -> Result<()> {
        self.con = open(&self.cfg, name).await?;
        self.db = name.to_string();
        Ok(())
    }

    pub async fn list_objects(&self) -> Result<Vec<DbObject>> {
        Ok(vec![DbObject {
            schema: None,
            name: "keys".into(),
            kind: ObjectKind::Keys,
        }])
    }

    pub fn structure(&self, _obj: &DbObject) -> Result<Structure> {
        let col = |name: &str, ty: &str, pk: bool| ColumnInfo {
            name: name.into(),
            data_type: ty.into(),
            nullable: !pk,
            default: None,
            is_pk: pk,
        };
        Ok(Structure {
            columns: vec![
                col("key", "string", true),
                col("type", "string", false),
                col("ttl", "integer", false),
                col("value", "any", false),
            ],
            indexes: QueryResult::default(),
            ..Default::default()
        })
    }

    pub async fn fetch_table(&self, q: &TableQuery) -> Result<(QueryResult, Option<u64>)> {
        let mut con = self.con.clone();
        let mut pattern = "*".to_string();
        for f in &q.filters {
            if f.column != "key" {
                continue;
            }
            match f.op {
                FilterOp::Eq => pattern = glob_escape(&f.value),
                FilterOp::Contains => pattern = format!("*{}*", glob_escape(&f.value)),
                _ => bail!("Redis key filters support only = and contains"),
            }
        }
        let mut keys: Vec<String> = Vec::new();
        let mut cursor: u64 = 0;
        loop {
            let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(1000)
                .query_async(&mut con)
                .await?;
            keys.extend(batch);
            cursor = next;
            if cursor == 0 || keys.len() >= SCAN_CAP {
                break;
            }
        }
        keys.sort();
        keys.dedup();
        if let Some((col, asc)) = &q.order {
            if col == "key" && !asc {
                keys.reverse();
            }
        }
        let total = keys.len() as u64;
        let page: Vec<String> = keys.into_iter().skip(q.offset).take(q.limit).collect();

        let mut pipe = redis::pipe();
        for k in &page {
            pipe.cmd("TYPE").arg(k).cmd("TTL").arg(k);
        }
        let meta: Vec<Value> = if page.is_empty() {
            vec![]
        } else {
            pipe.query_async(&mut con).await?
        };

        let mut rows = Vec::with_capacity(page.len());
        for (i, k) in page.iter().enumerate() {
            let ty = render(&meta[i * 2]).unwrap_or_default();
            let ttl = render(&meta[i * 2 + 1]);
            let mut c = redis::cmd(match ty.as_str() {
                "string" => "GET",
                "list" => "LRANGE",
                "set" => "SMEMBERS",
                "zset" => "ZRANGE",
                "hash" => "HGETALL",
                "stream" => "XLEN",
                _ => "TYPE",
            });
            c.arg(k);
            match ty.as_str() {
                "list" => {
                    c.arg(0).arg(99);
                }
                "zset" => {
                    c.arg(0).arg(99).arg("WITHSCORES");
                }
                _ => {}
            }
            let v: Value = c.query_async(&mut con).await.unwrap_or(Value::Nil);
            let value = if ty == "stream" {
                render(&v).map(|n| format!("stream ({n} entries)"))
            } else {
                render(&v)
            };
            rows.push(vec![Some(k.clone()), Some(ty), ttl, value]);
        }
        let res = QueryResult {
            columns: vec!["key".into(), "type".into(), "ttl".into(), "value".into()],
            rows,
            ..Default::default()
        };
        Ok((res, Some(total)))
    }

    pub async fn execute(&self, text: &str) -> Result<Vec<QueryResult>> {
        let mut con = self.con.clone();
        let mut out = Vec::new();
        for line in text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let w = words(line);
            let Some((name, args)) = w.split_first() else {
                continue;
            };
            let start = Instant::now();
            let mut c = redis::cmd(name);
            for a in args {
                c.arg(a);
            }
            let v: Value = c.query_async(&mut con).await?;
            let mut res = QueryResult {
                statement: line.to_string(),
                ..Default::default()
            };
            match &v {
                Value::Array(items) | Value::Set(items) => {
                    res.columns = vec!["#".into(), "value".into()];
                    res.rows = items
                        .iter()
                        .enumerate()
                        .map(|(i, x)| vec![Some(i.to_string()), render(x)])
                        .collect();
                }
                Value::Map(pairs) => {
                    res.columns = vec!["field".into(), "value".into()];
                    res.rows = pairs
                        .iter()
                        .map(|(k, x)| vec![render(k), render(x)])
                        .collect();
                }
                other => {
                    res.columns = vec!["result".into()];
                    res.rows = vec![vec![render(other)]];
                }
            }
            res.elapsed_ms = start.elapsed().as_millis();
            out.push(res);
        }
        Ok(out)
    }

    pub async fn commit(&self, changes: &[Change]) -> Result<()> {
        let mut con = self.con.clone();
        let key_of = |key: &[(String, Option<String>)]| {
            key.iter()
                .find(|(c, _)| c == "key")
                .and_then(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let mut pipe = redis::pipe();
        pipe.atomic();
        // Keys this commit will create, so two changes can't claim one name.
        let mut claimed = std::collections::HashSet::new();
        let mut ensure_free = async |k: &str, con: &mut MultiplexedConnection| -> Result<()> {
            if k.is_empty() {
                bail!("keys can't be empty");
            }
            let exists: bool = redis::cmd("EXISTS").arg(k).query_async(con).await?;
            if exists || !claimed.insert(k.to_string()) {
                bail!("key {k:?} already exists; nothing was changed");
            }
            Ok(())
        };
        for ch in changes {
            match ch {
                Change::Update { key, set } => {
                    let k = key_of(key);
                    // Value / TTL edits apply to the current name; the rename
                    // goes last, or they'd land on the old (now gone) key.
                    let mut rename_to = None;
                    for (col, v) in set {
                        match col.as_str() {
                            "value" => {
                                let ty: String =
                                    redis::cmd("TYPE").arg(&k).query_async(&mut con).await?;
                                if ty != "string" {
                                    bail!(
                                        "editing {ty} values in the grid is not supported yet; use the query editor"
                                    );
                                }
                                pipe.cmd("SET")
                                    .arg(&k)
                                    .arg(v.clone().unwrap_or_default())
                                    .arg("KEEPTTL");
                            }
                            "ttl" => match v.as_deref().and_then(|s| s.parse::<i64>().ok()) {
                                Some(n) if n > 0 => {
                                    pipe.cmd("EXPIRE").arg(&k).arg(n);
                                }
                                _ => {
                                    pipe.cmd("PERSIST").arg(&k);
                                }
                            },
                            "key" => rename_to = Some(v.clone().unwrap_or_default()),
                            _ => bail!("column {col} is read-only"),
                        }
                    }
                    if let Some(new) = rename_to.filter(|n| *n != k) {
                        ensure_free(&new, &mut con).await?;
                        // NX: never overwrite, even if the key appeared just now.
                        pipe.cmd("RENAMENX").arg(&k).arg(&new);
                    }
                }
                Change::Insert { values } => {
                    let get = |c: &str| {
                        values
                            .iter()
                            .find(|(n, _)| n == c)
                            .and_then(|(_, v)| v.clone())
                    };
                    let Some(k) = get("key") else {
                        bail!("new rows need a key")
                    };
                    ensure_free(&k, &mut con).await?;
                    pipe.cmd("SET")
                        .arg(&k)
                        .arg(get("value").unwrap_or_default())
                        .arg("NX");
                    if let Some(n) = get("ttl")
                        .and_then(|s| s.parse::<i64>().ok())
                        .filter(|n| *n > 0)
                    {
                        pipe.cmd("EXPIRE").arg(&k).arg(n);
                    }
                }
                Change::Delete { key } => {
                    pipe.cmd("DEL").arg(key_of(key));
                }
            }
        }
        pipe.query_async::<Value>(&mut con).await?;
        Ok(())
    }
}

pub fn preview(changes: &[Change]) -> String {
    let key_of = |key: &[(String, Option<String>)]| {
        key.iter()
            .find(|(c, _)| c == "key")
            .and_then(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let mut lines = vec!["MULTI".to_string()];
    for ch in changes {
        match ch {
            Change::Update { key, set } => {
                // Same order as `commit`: the rename goes last.
                let mut rename = None;
                for (col, v) in set {
                    let v = v.clone().unwrap_or_default();
                    match col.as_str() {
                        "value" => lines.push(format!("SET {:?} {v:?} KEEPTTL", key_of(key))),
                        "ttl" => lines.push(format!("EXPIRE {:?} {v}", key_of(key))),
                        _ => rename = Some(format!("RENAMENX {:?} {v:?}", key_of(key))),
                    }
                }
                lines.extend(rename);
            }
            Change::Insert { values } => {
                let get = |c: &str| {
                    values
                        .iter()
                        .find(|(n, _)| n == c)
                        .and_then(|(_, v)| v.clone())
                        .unwrap_or_default()
                };
                lines.push(format!("SET {:?} {:?} NX", get("key"), get("value")));
            }
            Change::Delete { key } => lines.push(format!("DEL {:?}", key_of(key))),
        }
    }
    lines.push("EXEC".into());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn words_handles_quotes() {
        assert_eq!(
            super::words(r#"SET "a b" 'c d' e"#),
            vec!["SET", "a b", "c d", "e"]
        );
        assert_eq!(super::words(r#"SET k """#), vec!["SET", "k", ""]);
    }
}
