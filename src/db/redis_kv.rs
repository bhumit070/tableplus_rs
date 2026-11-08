//! Redis: keys are shown as one "keys" table with key / type / ttl / value
//! columns. The editor runs one command per line.

use std::time::Instant;

use anyhow::{Result, bail};
use redis::Value;
use redis::aio::ConnectionManager;

use super::{
    Change, ColumnInfo, ConnectionConfig, DatabaseStats, DbObject, FilterOp, ObjectKind,
    QueryResult, Structure, TableQuery,
};
use anyhow::Error;
use redis::{
    AsyncConnectionConfig, Client, ErrorKind, RedisError, aio::ConnectionManagerConfig, cmd, pipe,
};
use serde_json::{Map, Value as SerdeJsonValue};

/// SCAN stops after this many keys so huge keyspaces stay responsive.
const SCAN_CAP: usize = 10_000;

pub struct RedisConn {
    cfg: ConnectionConfig,
    /// Reconnects by itself after the server restarts or the network drops.
    pub(super) con: ConnectionManager,
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

async fn open(cfg: &ConnectionConfig, db: &str) -> Result<ConnectionManager> {
    let scheme = if cfg.ssl { "rediss" } else { "redis" };
    let auth = match (cfg.user.is_empty(), cfg.password.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!(":{}@", pct(&cfg.password)),
        _ => format!("{}:{}@", pct(&cfg.user), pct(&cfg.password)),
    };
    let db = if db.is_empty() { "0" } else { db };
    let url = format!("{scheme}://{auth}{}:{}/{db}", cfg.host, cfg.port);
    let at = format!("{}:{}", cfg.host, cfg.port);
    let fail = |e: RedisError| Error::new(e).context(format!("couldn't connect to {at}"));
    // RESP3 returns hashes as maps; servers older than Redis 6 only speak
    // RESP2 and reject the HELLO handshake.
    let mut client = Client::open(format!("{url}?protocol=resp3"))?;
    // One plain connection first, so a down server or bad password fails
    // fast with its own reason (the manager retries quietly for minutes).
    let probe_cfg = || {
        AsyncConnectionConfig::new()
            .set_connection_timeout(std::time::Duration::from_secs(15))
            .set_response_timeout(std::time::Duration::from_secs(15))
    };
    if let Err(e) = client
        .get_multiplexed_async_connection_with_config(&probe_cfg())
        .await
    {
        let old_server = e.kind() == ErrorKind::ResponseError || e.to_string().contains("HELLO");
        if !old_server {
            return Err(fail(e));
        }
        client = Client::open(url)?;
        client
            .get_multiplexed_async_connection_with_config(&probe_cfg())
            .await
            .map_err(fail)?;
    }
    let config = ConnectionManagerConfig::new()
        .set_connection_timeout(std::time::Duration::from_secs(15))
        // Nothing waits forever on a server that stopped answering.
        .set_response_timeout(std::time::Duration::from_secs(30))
        .set_number_of_retries(2);
    ConnectionManager::new_with_config(client, config)
        .await
        .map_err(fail)
}

/// Commands that block or turn the connection into a subscriber: on the
/// shared connection they would freeze every other view.
const BLOCKING: &[&str] = &[
    "BLPOP",
    "BRPOP",
    "BLMOVE",
    "BRPOPLPUSH",
    "BZPOPMIN",
    "BZPOPMAX",
    "BLMPOP",
    "BZMPOP",
    "XREAD",
    "XREADGROUP",
    "WAIT",
    "WAITAOF",
    "SUBSCRIBE",
    "PSUBSCRIBE",
    "SSUBSCRIBE",
    "MONITOR",
    "SYNC",
    "PSYNC",
];

/// `db3:keys=12,expires=1,avg_ttl=0` lines → database "3" with 12 keys.
fn keyspace_counts(info: &str) -> Vec<DatabaseStats> {
    info.lines()
        .filter_map(|line| {
            let (database, fields) = line.trim().strip_prefix("db")?.split_once(':')?;
            let keys = fields
                .split(',')
                .find_map(|field| field.strip_prefix("keys="))?
                .parse::<u64>()
                .ok()?;
            Some(DatabaseStats {
                name: database.to_string(),
                size_bytes: None,
                objects: Some(keys),
            })
        })
        .collect()
}

pub fn render(v: &Value) -> Option<String> {
    Some(match v {
        Value::Nil => return None,
        Value::Int(n) => n.to_string(),
        Value::BulkString(b) => String::from_utf8_lossy(b).into_owned(),
        Value::SimpleString(s) => s.clone(),
        // RESP3 replies to INFO and similar.
        Value::VerbatimString { text, .. } => text.clone(),
        Value::Okay => "OK".into(),
        Value::Double(d) => d.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Array(items) | Value::Set(items) => {
            let parts: Vec<SerdeJsonValue> = items
                .iter()
                .map(|i| {
                    render(i)
                        .map(SerdeJsonValue::String)
                        .unwrap_or(SerdeJsonValue::Null)
                })
                .collect();
            SerdeJsonValue::Array(parts).to_string()
        }
        Value::Map(pairs) => {
            let obj: Map<String, SerdeJsonValue> = pairs
                .iter()
                .map(|(k, v)| {
                    (
                        render(k).unwrap_or_default(),
                        render(v)
                            .map(SerdeJsonValue::String)
                            .unwrap_or(SerdeJsonValue::Null),
                    )
                })
                .collect();
            SerdeJsonValue::Object(obj).to_string()
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
        cmd("PING").query_async::<String>(&mut con).await?;
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
        let me: i64 = cmd("CLIENT").arg("ID").query_async(&mut con).await?;
        let list: String = cmd("CLIENT").arg("LIST").query_async(&mut con).await?;
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
        let n: i64 = cmd("CLIENT")
            .arg("KILL")
            .arg("ID")
            .arg(id)
            .query_async(&mut con)
            .await?;
        if n == 0 {
            bail!("no such client");
        }
        Ok(())
    }

    pub async fn databases(&self) -> Result<Vec<String>> {
        let mut con = self.con.clone();
        let n = cmd("CONFIG")
            .arg("GET")
            .arg("databases")
            .query_async::<Vec<String>>(&mut con)
            .await
            .ok()
            .and_then(|v| v.get(1)?.parse::<usize>().ok())
            .unwrap_or(16);
        Ok((0..n).map(|i| i.to_string()).collect())
    }

    /// Key counts per database, from INFO keyspace (`db0:keys=3,expires=0,…`).
    pub async fn database_stats(&self) -> Result<Vec<DatabaseStats>> {
        let mut con = self.con.clone();
        let info: String = cmd("INFO").arg("keyspace").query_async(&mut con).await?;
        Ok(keyspace_counts(&info))
    }

    pub async fn use_database(&mut self, name: &str) -> Result<()> {
        self.con = open(&self.cfg, name).await?;
        self.db = name.to_string();
        Ok(())
    }

    /// "keys" (everything) plus one entry per key namespace — the part
    /// before the first `:` — found in a sample of up to 10,000 keys, so
    /// `user:1`, `user:2` show up as `user:*`.
    pub async fn list_objects(&self) -> Result<Vec<DbObject>> {
        let mut con = self.con.clone();
        let mut prefixes = std::collections::BTreeSet::new();
        let (mut cursor, mut seen) = (0u64, 0usize);
        loop {
            let (next, batch): (u64, Vec<String>) = cmd("SCAN")
                .arg(cursor)
                .arg("COUNT")
                .arg(1000)
                .query_async(&mut con)
                .await?;
            seen += batch.len();
            for k in batch {
                if let Some((p, _)) = k.split_once(':') {
                    prefixes.insert(format!("{}:*", glob_escape(p)));
                }
            }
            cursor = next;
            if cursor == 0 || seen >= 10_000 {
                break;
            }
        }
        let key = |name: String| DbObject {
            schema: None,
            name,
            kind: ObjectKind::Keys,
        };
        Ok(std::iter::once(key("keys".into()))
            .chain(prefixes.into_iter().map(key))
            .collect())
    }

    pub fn structure(&self, _obj: &DbObject) -> Result<Structure> {
        let col = |name: &str, ty: &str, pk: bool| ColumnInfo {
            name: name.into(),
            data_type: ty.into(),
            nullable: !pk,
            default: None,
            is_pk: pk,
            enum_values: vec![],
        };
        Ok(Structure {
            columns: vec![
                col("key", "string", true),
                // Shown as a coloured badge (grid Kind::KeyType).
                col("type", "redis type", false),
                col("ttl", "integer", false),
                col("value", "any", false),
            ],
            indexes: QueryResult::default(),
            ..Default::default()
        })
    }

    pub async fn fetch_table(&self, q: &TableQuery) -> Result<(QueryResult, Option<u64>)> {
        let mut con = self.con.clone();
        // A namespace entry ("user:*") limits the keys to that prefix.
        let base = if q.object.name == "keys" {
            String::new()
        } else {
            q.object.name.trim_end_matches('*').to_string()
        };
        let mut pattern = format!("{base}*");
        for f in &q.filters {
            if f.column != "key" {
                continue;
            }
            match f.op {
                FilterOp::Eq => pattern = glob_escape(&f.value),
                FilterOp::Contains => pattern = format!("{base}*{}*", glob_escape(&f.value)),
                _ => bail!("Redis key filters support only = and contains"),
            }
        }
        let mut keys: Vec<String> = Vec::new();
        let mut cursor: u64 = 0;
        loop {
            let (next, batch): (u64, Vec<String>) = cmd("SCAN")
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

        let mut pipe = pipe();
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
            let mut c = cmd(match ty.as_str() {
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
            let v: Value = match c.query_async(&mut con).await {
                Ok(v) => v,
                // The key changed type or vanished between SCAN and here.
                Err(e) if e.kind() == ErrorKind::TypeError || e.code() == Some("WRONGTYPE") => {
                    Value::Nil
                }
                Err(e) => return Err(e.into()),
            };
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
            if BLOCKING.contains(&name.to_ascii_uppercase().as_str()) {
                bail!(
                    "{} blocks the connection, which every view here shares; use redis-cli for it",
                    name.to_ascii_uppercase()
                );
            }
            let start = Instant::now();
            let mut c = cmd(name);
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
        let mut pipe = pipe();
        pipe.atomic();
        // Position of each NX command in the reply, and its key: a 0 / nil
        // there means the name was taken between our check and EXEC.
        let mut nx: Vec<(usize, String)> = vec![];
        let mut n = 0usize;
        // Keys this commit will create, so two changes can't claim one name.
        let mut claimed = std::collections::HashSet::new();
        let mut ensure_free = async |k: &str, con: &mut ConnectionManager| -> Result<()> {
            if k.is_empty() {
                bail!("keys can't be empty");
            }
            let exists: bool = cmd("EXISTS").arg(k).query_async(con).await?;
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
                                let ty: String = cmd("TYPE").arg(&k).query_async(&mut con).await?;
                                if ty != "string" {
                                    bail!(
                                        "editing {ty} values in the grid is not supported yet; use the query editor"
                                    );
                                }
                                pipe.cmd("SET")
                                    .arg(&k)
                                    .arg(v.clone().unwrap_or_default())
                                    .arg("KEEPTTL");
                                n += 1;
                            }
                            "ttl" => match v.as_deref().and_then(|s| s.parse::<i64>().ok()) {
                                Some(secs) if secs > 0 => {
                                    pipe.cmd("EXPIRE").arg(&k).arg(secs);
                                    n += 1;
                                }
                                _ => {
                                    pipe.cmd("PERSIST").arg(&k);
                                    n += 1;
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
                        nx.push((n, new));
                        n += 1;
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
                    // TTL in the same SET: a separate EXPIRE would also hit
                    // the key if someone else created it first.
                    let cmd = pipe
                        .cmd("SET")
                        .arg(&k)
                        .arg(get("value").unwrap_or_default())
                        .arg("NX");
                    if let Some(secs) = get("ttl")
                        .and_then(|s| s.parse::<i64>().ok())
                        .filter(|s| *s > 0)
                    {
                        cmd.arg("EX").arg(secs);
                    }
                    nx.push((n, k.clone()));
                    n += 1;
                }
                Change::Delete { key } => {
                    pipe.cmd("DEL").arg(key_of(key));
                    n += 1;
                }
            }
        }
        let replies: Vec<Value> = pipe.query_async(&mut con).await?;
        let lost: Vec<&str> = nx
            .iter()
            .filter(|(i, _)| matches!(replies.get(*i), Some(Value::Nil) | Some(Value::Int(0))))
            .map(|(_, k)| k.as_str())
            .collect();
        if !lost.is_empty() {
            bail!(
                "these keys were created by someone else meanwhile, so they weren't written: {}. The other changes were applied.",
                lost.join(", ")
            );
        }
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
    fn reads_keyspace_counts() {
        let counts = super::keyspace_counts(
            "# Keyspace\r\ndb0:keys=3,expires=0,avg_ttl=0\r\ndb5:keys=12,expires=1\r\n",
        );
        let pairs: Vec<(String, Option<u64>)> = counts
            .into_iter()
            .map(|stats| (stats.name, stats.objects))
            .collect();
        assert_eq!(pairs, [("0".into(), Some(3)), ("5".into(), Some(12))]);
    }

    #[test]
    fn words_handles_quotes() {
        assert_eq!(
            super::words(r#"SET "a b" 'c d' e"#),
            vec!["SET", "a b", "c d", "e"]
        );
        assert_eq!(super::words(r#"SET k """#), vec!["SET", "k", ""]);
    }
}
