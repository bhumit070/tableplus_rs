//! Postgres / MySQL / SQLite via sqlx.
//!
//! Reads go through `raw_sql` (simple/text protocol) so every value arrives as
//! text and can be shown without per-type decoding. Writes use prepared
//! statements with bound parameters.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures::TryStreamExt;
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlRow, MySqlSslMode};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgRow, PgSslMode};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{
    AssertSqlSafe, Column, Connection, Either, Executor, Row, SqlSafeStr, Statement, TypeInfo,
    ValueRef,
};

use super::cancel::{CANCELLED, Cancel};
use super::split::split_statements;
use super::{
    Change, ColumnInfo, ConnectionConfig, DbObject, Driver, FilterOp, ForeignKey, ObjectKind,
    QueryResult, Structure, TableQuery, sql_literal,
};
use anyhow::{Error, anyhow};
use redis::RedisError;
use serde_json::from_str;
use sqlx::{Error as SqlxError, MySqlConnection, PgConnection, query, raw_sql};
use tokio::{select, time::timeout};

enum Pool {
    Pg(PgPool),
    My(MySqlPool),
    Lite(SqlitePool),
}

pub struct SqlConn {
    cfg: ConnectionConfig,
    pool: Pool,
    database: String,
    /// Postgres schema put first on `search_path`; `None` = server default.
    schema: Option<String>,
}

/// SSL checkbox → mode. "Verify" checks the certificate and host name.
fn pg_ssl_mode(cfg: &ConnectionConfig) -> PgSslMode {
    match (cfg.ssl, cfg.ssl_verify) {
        (true, true) => PgSslMode::VerifyFull,
        (true, false) => PgSslMode::Require,
        (false, _) => PgSslMode::Prefer,
    }
}

fn my_ssl_mode(cfg: &ConnectionConfig) -> MySqlSslMode {
    match (cfg.ssl, cfg.ssl_verify) {
        (true, true) => MySqlSslMode::VerifyIdentity,
        (true, false) => MySqlSslMode::Required,
        (false, _) => MySqlSslMode::Preferred,
    }
}

/// How long a server may take to accept a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Opens (and closes) one connection, so failures carry the server's own
/// reason — refused, bad password, unknown database, TLS — with the address.
async fn probe<C: Connection>(
    cfg: &ConnectionConfig,
    connect: impl std::future::Future<Output = std::result::Result<C, SqlxError>>,
) -> Result<()> {
    let at = format!("{}:{}", cfg.host, cfg.port);
    match timeout(CONNECT_TIMEOUT, connect).await {
        Err(_) => bail!("no answer from {at} within {} s", CONNECT_TIMEOUT.as_secs()),
        Ok(Err(e)) => Err(Error::new(e).context(format!("couldn't connect to {at}"))),
        Ok(Ok(c)) => {
            let _ = c.close().await;
            Ok(())
        }
    }
}

/// A pool that can't get a connection says only "pool timed out": say
/// what that means for the user.
pub(super) fn explain_lost(e: Error) -> Error {
    let lost = e.downcast_ref::<SqlxError>().is_some_and(|e| {
        matches!(
            e,
            SqlxError::PoolTimedOut | SqlxError::PoolClosed | SqlxError::Io(_)
        )
    }) || e.downcast_ref::<RedisError>().is_some_and(|e| {
        e.is_timeout() || e.is_io_error() || e.is_connection_dropped() || e.is_connection_refusal()
    });
    if lost {
        e.context("Lost the connection to the server (it may be down or unreachable). Try again or reconnect")
    } else {
        e
    }
}

fn pg_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Postgres stores FK actions as one letter; MySQL / SQLite spell them out.
fn fk_action(code: &str) -> String {
    match code {
        "a" => "NO ACTION",
        "r" => "RESTRICT",
        "c" => "CASCADE",
        "n" => "SET NULL",
        "d" => "SET DEFAULT",
        other => other,
    }
    .to_string()
}

// ---- row decoding ---------------------------------------------------------

/// The values of a MySQL `enum('a','it''s')` column type; empty for any
/// other type.
fn mysql_enum(ty: &str) -> Vec<String> {
    let Some(list) = ty.strip_prefix("enum(").and_then(|t| t.strip_suffix(')')) else {
        return vec![];
    };
    let (mut out, mut cur, mut quoted) = (vec![], String::new(), false);
    let mut chars = list.chars().peekable();
    while let Some(ch) = chars.next() {
        match (ch, quoted) {
            ('\'', true) if chars.peek() == Some(&'\'') => {
                chars.next();
                cur.push('\'');
            }
            ('\'', true) => {
                out.push(std::mem::take(&mut cur));
                quoted = false;
            }
            ('\'', false) => quoted = true,
            (ch, true) => cur.push(ch),
            _ => {}
        }
    }
    out
}

/// Columns limited to a list by a `CHECK`, from a table's DDL: `CHECK
/// (status IN ('a', 'b'))` (SQLite, MySQL) or `CHECK ((status = ANY
/// (ARRAY['a'::text, 'b'::text])))` (Postgres). They edit like enums.
/// Constraints that do anything more are ignored.
pub(crate) fn check_in_lists(ddl: &str) -> Vec<(String, Vec<String>)> {
    let lower = ddl.to_ascii_lowercase();
    let mut out = vec![];
    let mut from = 0;
    while let Some(at) = lower[from..].find("check") {
        let start = from + at + "check".len();
        from = start;
        // A word, not part of one ("checked").
        let before = lower[..start - 5].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let rest = ddl[start..].trim_start();
        if !rest.starts_with('(') {
            continue;
        }
        let Some(expr) = balanced(rest) else { continue };
        if let Some(found) = in_list(expr) {
            out.push(found);
        }
    }
    out
}

/// The text inside the parentheses `s` starts with (quotes respected).
fn balanced(s: &str) -> Option<&str> {
    if !s.starts_with('(') {
        return None;
    }
    let (mut depth, mut quote, mut escaped) = (0usize, None::<char>, false);
    for (i, ch) in s.char_indices() {
        match (quote, ch) {
            // MySQL escapes quotes with a backslash: 'it\'s'.
            (Some(_), _) if escaped => escaped = false,
            (Some(_), '\\') => escaped = true,
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"' | '`') => quote = Some(ch),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[1..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Strips whole-expression parentheses: "((a))" → "a".
fn unwrap_parens(mut s: &str) -> &str {
    loop {
        let t = s.trim();
        match balanced(t) {
            Some(inner) if t.len() == inner.len() + 2 => s = inner,
            _ => return t,
        }
    }
}

/// Splits at the first `sep` (lower-case) outside quotes and parentheses.
fn split_top<'a>(s: &'a str, sep: &str) -> Option<(&'a str, &'a str)> {
    let lower = s.to_ascii_lowercase();
    let (mut depth, mut quote, mut escaped) = (0i32, None::<char>, false);
    for (i, ch) in s.char_indices() {
        match (quote, ch) {
            (Some(_), _) if escaped => {
                escaped = false;
                continue;
            }
            (Some(_), '\\') => {
                escaped = true;
                continue;
            }
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => continue,
            (None, '\'' | '"' | '`') => quote = Some(ch),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth -= 1,
            _ => {}
        }
        if depth == 0 && quote.is_none() && lower[i..].starts_with(sep) {
            return Some((&s[..i], &s[i + sep.len()..]));
        }
    }
    None
}

/// A bare column name: `status`, "status", `(status)::text`.
fn column_name(s: &str) -> Option<String> {
    let s = unwrap_parens(s);
    let s = unwrap_parens(s.split("::").next()?);
    let name = s.trim_matches(|c| matches!(c, '"' | '`' | '[' | ']'));
    (!name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$'))
    .then(|| name.to_string())
}

/// `'a', 'it''s', _utf8mb4'b', 'c'::text` → the strings; `None` if any item
/// isn't a string literal.
fn literals(s: &str) -> Option<Vec<String>> {
    let mut out = vec![];
    let mut rest = s.trim();
    while !rest.is_empty() {
        // Charset introducers (MySQL) and national strings.
        let lit_start = rest.find('\'')?;
        let prefix = &rest[..lit_start];
        if !prefix.is_empty() && !prefix.starts_with('_') && !prefix.eq_ignore_ascii_case("n") {
            return None;
        }
        let mut value = String::new();
        let mut chars = rest[lit_start + 1..].char_indices().peekable();
        let mut end = None;
        while let Some((i, ch)) = chars.next() {
            match ch {
                '\'' if chars.peek().is_some_and(|(_, c)| *c == '\'') => {
                    chars.next();
                    value.push('\'');
                }
                '\'' => {
                    end = Some(lit_start + 1 + i + 1);
                    break;
                }
                '\\' => {
                    if let Some((_, c)) = chars.next() {
                        value.push(c);
                    }
                }
                c => value.push(c),
            }
        }
        out.push(value);
        rest = rest[end?..].trim_start();
        // A cast after the literal: 'a'::character varying.
        if let Some(r) = rest.strip_prefix("::") {
            rest = r.trim_start_matches(|c: char| c != ',').trim_start();
        }
        match rest.strip_prefix(',') {
            Some(r) => rest = r.trim_start(),
            None if rest.is_empty() => {}
            None => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// One `col IN (…)` or `col = ANY (ARRAY[…])` and nothing else.
fn in_list(expr: &str) -> Option<(String, Vec<String>)> {
    let expr = unwrap_parens(expr);
    if let Some((lhs, rhs)) = split_top(expr, " in ") {
        let rhs = rhs.trim();
        let inner = balanced(rhs).filter(|i| i.len() + 2 == rhs.len())?;
        return Some((column_name(lhs)?, literals(inner)?));
    }
    let (lhs, rhs) = split_top(expr, "= any")?;
    let rhs = rhs.trim();
    let lower = rhs.to_ascii_lowercase();
    let open = lower.find("array[")? + "array[".len();
    let close = rhs.rfind(']')?;
    Some((column_name(lhs)?, literals(&rhs[open..close])?))
}

fn pg_row(row: &PgRow) -> Vec<Option<String>> {
    (0..row.len())
        .map(|i| {
            let v = row.try_get_raw(i).ok()?;
            if v.is_null() {
                return None;
            }
            let is_bool = v.type_info().name() == "BOOL";
            let s = String::from_utf8_lossy(v.as_bytes().ok()?).into_owned();
            Some(match (is_bool, s.as_str()) {
                (true, "t") => "true".into(),
                (true, "f") => "false".into(),
                _ => s,
            })
        })
        .collect()
}

fn my_row(row: &MySqlRow) -> Vec<Option<String>> {
    (0..row.len())
        .map(|i| {
            row.try_get_unchecked::<Option<Vec<u8>>, _>(i)
                .ok()
                .flatten()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
        })
        .collect()
}

fn lite_row(row: &SqliteRow) -> Vec<Option<String>> {
    (0..row.len())
        .map(|i| {
            let v = row.try_get_raw(i).ok()?;
            if v.is_null() {
                return None;
            }
            match v.type_info().name() {
                "INTEGER" => row
                    .try_get_unchecked::<i64, _>(i)
                    .ok()
                    .map(|n| n.to_string()),
                "REAL" => row
                    .try_get_unchecked::<f64, _>(i)
                    .ok()
                    .map(|n| n.to_string()),
                "BLOB" => row.try_get_unchecked::<Vec<u8>, _>(i).ok().map(|b| {
                    format!(
                        "0x{}",
                        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
                    )
                }),
                _ => row.try_get_unchecked::<String, _>(i).ok(),
            }
        })
        .collect()
}

/// Runs one statement through the text protocol on any of the three pools.
macro_rules! run_raw {
    ($pool:expr, $sql:expr, $decode:ident) => {{
        let sql: String = $sql;
        let mut stream = raw_sql(AssertSqlSafe(sql.clone())).fetch_many($pool);
        let mut res = QueryResult {
            statement: sql.clone(),
            ..Default::default()
        };
        let mut affected: Option<u64> = None;
        while let Some(item) = stream.try_next().await? {
            match item {
                Either::Left(done) => {
                    *affected.get_or_insert(0) += done.rows_affected();
                }
                Either::Right(row) => {
                    if res.columns.is_empty() {
                        res.columns = row.columns().iter().map(|c| c.name().to_string()).collect();
                    }
                    res.rows.push($decode(&row));
                }
            }
        }
        drop(stream);
        if res.columns.is_empty() {
            // Empty result set: still show the column headers when we can.
            if let Ok(d) = $pool
                .prepare(AssertSqlSafe(sql.clone()).into_sql_str())
                .await
            {
                res.columns = d.columns().iter().map(|c| c.name().to_string()).collect();
            }
        }
        if res.columns.is_empty() {
            res.affected = affected;
        }
        res
    }};
}

/// What a batch of editor statements did to its connection.
#[derive(Default, Debug, PartialEq)]
struct SessionEffects {
    /// A transaction was started and not yet ended.
    tx_open: bool,
    /// Session state changed (SET, USE, temp tables…), so the connection
    /// must not go back to the shared pool.
    dirty: bool,
}

impl SessionEffects {
    fn observe(&mut self, stmt: &str) {
        let lower = stmt.to_ascii_lowercase();
        let words: Vec<&str> = lower.split_whitespace().take(3).collect();
        match words.as_slice() {
            ["begin", ..] | ["start", "transaction", ..] => self.tx_open = true,
            // `rollback to savepoint` keeps the transaction open.
            ["rollback", "to", ..] => {}
            ["commit", ..] | ["end", ..] | ["rollback", ..] | ["abort", ..] => self.tx_open = false,
            ["set", ..]
            | ["use", ..]
            | ["reset", ..]
            | ["declare", ..]
            | ["prepare", ..]
            | ["listen", ..]
            | ["lock", ..]
            | ["attach", ..]
            | ["detach", ..] => self.dirty = true,
            ["create", "temp" | "temporary", ..] | ["create", "local" | "global", ..] => {
                self.dirty = true
            }
            ["pragma", ..] if lower.contains('=') => self.dirty = true,
            _ => {}
        }
        if lower.contains("set_config(") {
            self.dirty = true;
        }
    }
}

/// Runs editor statements on one pooled connection, so `BEGIN … ROLLBACK`
/// and session settings apply to the statements that follow them.
/// Afterwards: a transaction left open is rolled back (and reported), and a
/// connection whose session changed or errored is closed rather than
/// returned to the pool, where it would leak that state into later reads.
macro_rules! run_batch {
    ($pool:expr, $stmts:expr, $decode:ident, $cancel:expr, $server:expr) => {{
        let stmts: Vec<String> = $stmts;
        let cancel: &Cancel = $cancel;
        let n = stmts.len();
        let mut conn = $pool.acquire().await?;
        // How to cancel this connection's statement from another one:
        // (query for its id, cancel statement with `{}` for the id).
        let server: Option<(&str, &str)> = $server;
        if let Some((id_sql, kill)) = server {
            let id = async { Ok::<_, Error>(run_raw!(&mut *conn, id_sql.to_string(), $decode)) }
                .await
                .ok()
                .and_then(|r| r.rows.first()?.first()?.as_deref()?.parse::<u64>().ok());
            if let Some(id) = id {
                let (pool, sql) = ($pool.clone(), kill.replace("{}", &id.to_string()));
                cancel.on_server_cancel(Box::new(move || {
                    Box::pin(async move {
                        raw_sql(AssertSqlSafe(sql))
                            .execute(&pool)
                            .await
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })
                }));
            }
        }
        let mut effects = SessionEffects::default();
        let mut out = Vec::new();
        let mut failed: Option<(usize, Error)> = None;
        for (i, stmt) in stmts.into_iter().enumerate() {
            effects.observe(&stmt);
            let start = Instant::now();
            let res: Result<QueryResult> = if cancel.is_cancelled() {
                Err(anyhow!(CANCELLED))
            } else {
                select! {
                    r = async { Ok(run_raw!(&mut *conn, stmt, $decode)) } => r,
                    _ = cancel.cancelled() => Err(anyhow!(CANCELLED)),
                }
            };
            // The server-side cancel makes the statement fail with its own
            // error, or (MySQL SLEEP) return early as if done: either way
            // it was cancelled.
            let res = if cancel.is_cancelled() {
                Err(anyhow!(CANCELLED))
            } else {
                res
            };
            match res {
                Ok(mut r) => {
                    r.elapsed_ms = start.elapsed().as_millis();
                    out.push(r);
                }
                Err(e) => {
                    failed = Some((i, e));
                    break;
                }
            }
        }
        let rolled_back = effects.tx_open;
        if cancel.is_cancelled() {
            // The connection may be mid-statement: drop it (closing the
            // socket also rolls back an open transaction server-side).
            drop(conn.detach());
        } else {
            if rolled_back {
                let _ = raw_sql("rollback").execute(&mut *conn).await;
            }
            if effects.dirty || rolled_back || failed.is_some() {
                let _ = conn.detach().close().await;
            }
        }
        let note = if rolled_back { " The open transaction was rolled back." } else { "" };
        if let Some((i, e)) = failed {
            let ran = if i > 0 && !rolled_back {
                format!(" The {i} statement(s) before it were applied.")
            } else {
                String::new()
            };
            if cancel.is_cancelled() {
                let e = anyhow!(cancel.message());
                if n == 1 && !rolled_back {
                    return Err(e);
                }
                return Err(e.context(format!("Stopped at statement {} of {n}.{ran}{note}", i + 1)));
            }
            return Err(e.context(format!("statement {} of {n} failed.{ran}{note}", i + 1)));
        }
        if rolled_back {
            bail!("BEGIN without COMMIT or ROLLBACK: nothing was saved.{note} End the batch with COMMIT; to keep the changes.");
        }
        out
    }};
}

/// Applies a list of (sql, params) in one transaction.
macro_rules! run_tx {
    ($pool:expr, $stmts:expr) => {{
        let mut tx = $pool.begin().await?;
        for (sql, params, expect_one) in $stmts {
            let mut q = query(AssertSqlSafe(sql.clone()));
            for p in params {
                q = q.bind(p);
            }
            let n = q
                .execute(&mut *tx)
                .await
                .with_context(|| sql.clone())?
                .rows_affected();
            if expect_one && n != 1 {
                // Rolls back on drop.
                bail!("{sql}\naffected {n} rows instead of 1; nothing was committed");
            }
        }
        tx.commit().await?;
    }};
}

impl SqlConn {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let pool = Self::open(cfg, &cfg.database, None).await?;
        let mut conn = SqlConn {
            cfg: cfg.clone(),
            pool,
            database: cfg.database.clone(),
            schema: None,
        };
        let cur = match conn.cfg.driver {
            Driver::Postgres => "select current_database()",
            Driver::MySql => "select coalesce(database(), '')",
            _ => "select 'main'",
        };
        conn.database = conn
            .raw(cur.into())
            .await?
            .rows
            .first()
            .and_then(|r| r[0].clone())
            .unwrap_or_default();
        Ok(conn)
    }

    async fn open(cfg: &ConnectionConfig, database: &str, schema: Option<&str>) -> Result<Pool> {
        let timeout = Duration::from_secs(10);
        let read_only = cfg.safety() == super::SafeMode::ReadOnly;
        Ok(match cfg.driver {
            Driver::Postgres => {
                let mut o = PgConnectOptions::new()
                    .host(&cfg.host)
                    .port(cfg.port)
                    .username(&cfg.user)
                    .password(&cfg.password)
                    .application_name("tabula")
                    .ssl_mode(pg_ssl_mode(cfg));
                if !database.is_empty() {
                    o = o.database(database);
                }
                let mut pool = PgPoolOptions::new()
                    .max_connections(4)
                    .acquire_timeout(timeout);
                let mut setup = vec![];
                if let Some(schema) = schema {
                    // Every pooled connection resolves unqualified names in
                    // the chosen schema first; `public` keeps extensions working.
                    setup.push(format!("set search_path to {}, public", pg_ident(schema)));
                }
                if read_only {
                    // The server refuses writes, whatever the editor's
                    // read/write guess says.
                    setup.push("set session characteristics as transaction read only".into());
                }
                if !setup.is_empty() {
                    let sql = setup.join("; ");
                    pool = pool.after_connect(move |conn, _| {
                        let sql = sql.clone();
                        Box::pin(async move {
                            raw_sql(AssertSqlSafe(sql))
                                .execute(&mut *conn)
                                .await
                                .map(|_| ())
                        })
                    });
                }
                // One direct connection first: the pool would retry quietly
                // and only report "pool timed out", hiding the real cause.
                probe(cfg, PgConnection::connect_with(&o)).await?;
                Pool::Pg(pool.connect_with(o).await?)
            }
            Driver::MySql => {
                let mut o = MySqlConnectOptions::new()
                    .host(&cfg.host)
                    .port(cfg.port)
                    .username(&cfg.user)
                    .password(&cfg.password)
                    .ssl_mode(my_ssl_mode(cfg));
                if !database.is_empty() {
                    o = o.database(database);
                }
                let mut pool = MySqlPoolOptions::new()
                    .max_connections(4)
                    .acquire_timeout(timeout);
                if read_only {
                    pool = pool.after_connect(|conn, _| {
                        Box::pin(async move {
                            raw_sql("set session transaction read only")
                                .execute(&mut *conn)
                                .await
                                .map(|_| ())
                        })
                    });
                }
                probe(cfg, MySqlConnection::connect_with(&o)).await?;
                Pool::My(pool.connect_with(o).await?)
            }
            Driver::Sqlite => {
                if cfg.file.is_empty() {
                    bail!("choose a SQLite database file");
                }
                // A typo or a moved file is an error, not a new empty database.
                if !std::path::Path::new(&cfg.file).is_file() {
                    bail!("SQLite file not found: {}", cfg.file);
                }
                let o = SqliteConnectOptions::new()
                    .filename(&cfg.file)
                    .create_if_missing(false)
                    .read_only(read_only);
                Pool::Lite(
                    SqlitePoolOptions::new()
                        .max_connections(4)
                        .acquire_timeout(timeout)
                        .connect_with(o)
                        .await?,
                )
            }
            _ => unreachable!(),
        })
    }

    async fn raw(&self, sql: String) -> Result<QueryResult> {
        Ok(match &self.pool {
            Pool::Pg(p) => run_raw!(p, sql, pg_row),
            Pool::My(p) => run_raw!(p, sql, my_row),
            Pool::Lite(p) => run_raw!(p, sql, lite_row),
        })
    }

    /// Up to `limit` rows of `obj` whose `key` starts with `text` or whose
    /// `labels` columns contain it (case-insensitive): (key, labels joined
    /// with " · "). For the foreign-key picker; only reads.
    pub async fn fk_matches(
        &self,
        obj: &DbObject,
        key: &str,
        labels: &[String],
        text: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let as_text = |col: &str| match self.cfg.driver {
            Driver::MySql => format!("cast({} as char)", self.q(col)),
            Driver::Postgres => format!("{}::text", self.q(col)),
            _ => format!("cast({} as text)", self.q(col)),
        };
        // LIKE wildcards in the typed text are literal.
        let esc = text
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let like = match self.cfg.driver {
            Driver::Postgres => "ilike",
            _ => "like",
        };
        // A backslash is an escape inside MySQL strings too.
        let escape = match self.cfg.driver {
            Driver::MySql => r"'\\'",
            _ => r"'\'",
        };
        let label = if labels.is_empty() {
            "''".to_string()
        } else {
            let parts: Vec<String> = labels.iter().map(|l| as_text(l)).collect();
            format!("concat_ws(' · ', {})", parts.join(", "))
        };
        let label = if self.cfg.driver == Driver::Sqlite && labels.len() > 1 {
            // SQLite has no concat_ws before 3.44.
            labels
                .iter()
                .map(|l| format!("coalesce({}, '')", as_text(l)))
                .collect::<Vec<_>>()
                .join(" || ' · ' || ")
        } else {
            label
        };
        // Aliased, so ORDER BY below sorts by the column, not its text.
        let mut sql = format!(
            "select {} as tabula_key, {label} as tabula_label from {}",
            as_text(key),
            self.full_name(obj)
        );
        if !text.is_empty() {
            let mut conds = vec![format!(
                "{} {like} {} escape {escape}",
                as_text(key),
                self.lit(&format!("{esc}%"))
            )];
            for l in labels {
                conds.push(format!(
                    "{} {like} {} escape {escape}",
                    as_text(l),
                    self.lit(&format!("%{esc}%"))
                ));
            }
            sql += &format!(" where {}", conds.join(" or "));
        }
        sql += &format!(" order by {} limit {limit}", self.q(key));
        let r = self.raw(sql).await?;
        Ok(r.rows
            .into_iter()
            .filter_map(|row| {
                let k = row.first().cloned().flatten()?;
                Some((k, row.get(1).cloned().flatten().unwrap_or_default()))
            })
            .collect())
    }

    fn q(&self, ident: &str) -> String {
        match self.cfg.driver {
            Driver::MySql => format!("`{}`", ident.replace('`', "``")),
            _ => format!("\"{}\"", ident.replace('"', "\"\"")),
        }
    }

    fn lit(&self, s: &str) -> String {
        match self.cfg.driver {
            Driver::MySql => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''")),
            _ => format!("'{}'", s.replace('\'', "''")),
        }
    }

    fn full_name(&self, obj: &DbObject) -> String {
        match (&obj.schema, self.cfg.driver) {
            (Some(s), Driver::Postgres) => format!("{}.{}", self.q(s), self.q(&obj.name)),
            _ => self.q(&obj.name),
        }
    }

    pub fn current_database(&self) -> String {
        self.database.clone()
    }

    pub async fn databases(&self) -> Result<Vec<String>> {
        let sql = match self.cfg.driver {
            Driver::Postgres => {
                "select datname from pg_database where not datistemplate order by 1"
            }
            Driver::MySql => "show databases",
            _ => return Ok(vec!["main".into()]),
        };
        Ok(self
            .raw(sql.into())
            .await?
            .rows
            .into_iter()
            .filter_map(|r| r[0].clone())
            .collect())
    }

    pub async fn use_database(&mut self, name: &str) -> Result<()> {
        if self.cfg.driver == Driver::Sqlite {
            return Ok(());
        }
        let pool = Self::open(&self.cfg, name, None).await?;
        self.close().await;
        self.pool = pool;
        self.database = name.to_string();
        self.schema = None;
        Ok(())
    }

    pub fn current_schema(&self) -> Option<String> {
        self.schema.clone()
    }

    /// Postgres only: reconnects so unqualified names resolve in `schema`.
    pub async fn set_schema(&mut self, schema: Option<String>) -> Result<()> {
        if self.cfg.driver != Driver::Postgres {
            return Ok(());
        }
        let pool = Self::open(&self.cfg, &self.database, schema.as_deref()).await?;
        self.close().await;
        self.pool = pool;
        self.schema = schema;
        Ok(())
    }

    /// (schema, table, column) for every column, for autocomplete.
    pub async fn all_columns(&self) -> Result<Vec<(String, String, String)>> {
        // Capped so a huge schema can't stall the editor.
        let sql = match self.cfg.driver {
            Driver::Postgres => {
                "select table_schema, table_name, column_name from information_schema.columns \
                 where table_schema not in ('pg_catalog', 'information_schema') \
                 order by table_name, ordinal_position limit 50000"
            }
            Driver::MySql => {
                "select table_schema, table_name, column_name from information_schema.columns \
                 where table_schema = database() order by table_name, ordinal_position limit 50000"
            }
            _ => {
                "select 'main', m.name, p.name from sqlite_master m join pragma_table_info(m.name) p \
                 where m.type in ('table', 'view') and m.name not like 'sqlite_%' order by m.name, p.cid"
            }
        };
        Ok(self
            .raw(sql.into())
            .await?
            .rows
            .into_iter()
            .filter_map(|r| Some((r[0].clone()?, r[1].clone()?, r[2].clone()?)))
            .collect())
    }

    pub async fn list_objects(&self) -> Result<Vec<DbObject>> {
        let sql = match self.cfg.driver {
            Driver::Postgres => {
                "select table_schema, table_name, table_type from information_schema.tables \
                 where table_schema not in ('pg_catalog', 'information_schema') order by 1, 2"
            }
            Driver::MySql => {
                "select table_schema, table_name, table_type from information_schema.tables \
                 where table_schema = database() order by 2"
            }
            _ => {
                "select 'main', name, type from sqlite_master \
                 where type in ('table', 'view') and name not like 'sqlite_%' order by 2"
            }
        };
        Ok(self
            .raw(sql.into())
            .await?
            .rows
            .into_iter()
            .map(|r| {
                let kind = r[2].as_deref().unwrap_or("").to_ascii_lowercase();
                DbObject {
                    schema: r[0].clone(),
                    name: r[1].clone().unwrap_or_default(),
                    kind: if kind.contains("view") {
                        ObjectKind::View
                    } else {
                        ObjectKind::Table
                    },
                }
            })
            .collect())
    }

    pub async fn structure(&self, obj: &DbObject) -> Result<Structure> {
        let schema = obj.schema.clone().unwrap_or_default();
        let (cols_sql, idx_sql) = match self.cfg.driver {
            Driver::Postgres => (
                format!(
                    "select a.attname, format_type(a.atttypid, a.atttypmod), (not a.attnotnull)::text, \
                     pg_get_expr(d.adbin, d.adrelid), \
                     (exists (select 1 from pg_index i where i.indrelid = a.attrelid and i.indisprimary \
                       and a.attnum = any(i.indkey)))::text, \
                     (select json_agg(e.enumlabel order by e.enumsortorder) from pg_enum e \
                       where e.enumtypid = a.atttypid)::text \
                     from pg_attribute a left join pg_attrdef d on d.adrelid = a.attrelid and d.adnum = a.attnum \
                     where a.attrelid = to_regclass({}) and a.attnum > 0 and not a.attisdropped order by a.attnum",
                    self.lit(&self.full_name(obj))
                ),
                format!(
                    "select indexname as name, indexdef as definition from pg_indexes \
                     where schemaname = {} and tablename = {} order by 1",
                    self.lit(&schema),
                    self.lit(&obj.name)
                ),
            ),
            Driver::MySql => (
                format!(
                    "select column_name, column_type, if(is_nullable = 'YES', 'true', 'false'), column_default, \
                     if(column_key = 'PRI', 'true', 'false') from information_schema.columns \
                     where table_schema = database() and table_name = {} order by ordinal_position",
                    self.lit(&obj.name)
                ),
                format!("show index from {}", self.q(&obj.name)),
            ),
            _ => (
                format!(
                    "select name, type, case \"notnull\" when 0 then 'true' else 'false' end, dflt_value, \
                     case when pk > 0 then 'true' else 'false' end from pragma_table_info({})",
                    self.lit(&obj.name)
                ),
                format!(
                    "select name, sql as definition from sqlite_master where type = 'index' and tbl_name = {}",
                    self.lit(&obj.name)
                ),
            ),
        };
        let columns: Vec<ColumnInfo> = self
            .raw(cols_sql)
            .await?
            .rows
            .into_iter()
            .map(|r| ColumnInfo {
                name: r[0].clone().unwrap_or_default(),
                data_type: r[1].clone().unwrap_or_default(),
                nullable: r[2].as_deref() == Some("true"),
                default: r[3].clone(),
                is_pk: r[4].as_deref() == Some("true"),
                // Postgres lists the labels in a sixth column; MySQL spells
                // them out in the column type.
                enum_values: match r.get(5).cloned().flatten() {
                    Some(json) => from_str(&json).unwrap_or_default(),
                    None => mysql_enum(r[1].as_deref().unwrap_or_default()),
                },
            })
            .collect();
        // Extras: when one can't be read (permissions, old servers…) the
        // rest still shows, with a note saying what's missing.
        let mut warnings = vec![];
        let mut part = |what: &str, e: Error| warnings.push(format!("{what}: {e:#}"));
        let indexes = self.raw(idx_sql).await.unwrap_or_else(|e| {
            part("Indexes", e);
            Default::default()
        });
        let foreign_keys = self.foreign_keys(obj).await.unwrap_or_else(|e| {
            part("Foreign keys", e);
            Default::default()
        });
        let ddl = self.ddl(obj, &columns).await.unwrap_or_else(|e| {
            part("SQL", e);
            Default::default()
        });
        // CHECK (col IN (…)) columns edit like enums too.
        let mut columns = columns;
        for (name, values) in check_in_lists(&ddl) {
            if let Some(c) = columns
                .iter_mut()
                .find(|c| c.name == name && c.enum_values.is_empty())
            {
                c.enum_values = values;
            }
        }
        Ok(Structure {
            columns,
            indexes,
            foreign_keys,
            ddl,
            warnings,
        })
    }

    /// Server sessions, first column = the id `kill` takes. Our own
    /// session is left out.
    pub async fn processes(&self) -> Result<QueryResult> {
        let sql = match self.cfg.driver {
            Driver::Postgres => {
                "select pid, usename as user, datname as database, \
                 coalesce(host(client_addr), 'local') as client, application_name as app, state, \
                 to_char(now() - query_start, 'HH24:MI:SS') as running_for, \
                 wait_event_type || coalesce(': ' || wait_event, '') as waiting_on, query \
                 from pg_stat_activity where pid <> pg_backend_pid() and backend_type = 'client backend' \
                 order by query_start nulls last"
            }
            Driver::MySql => {
                "select id, user, host as client, db as `database`, command, time as seconds, state, info as query \
                 from information_schema.processlist where id <> connection_id() order by time desc"
            }
            _ => bail!("SQLite has no server processes"),
        };
        self.raw(sql.into()).await
    }

    /// Stops a session's running statement, or (`whole`) the session.
    pub async fn kill(&self, id: &str, whole: bool) -> Result<()> {
        let id: u64 = id.parse().context("not a process id")?;
        let sql = match (self.cfg.driver, whole) {
            (Driver::Postgres, false) => format!("select pg_cancel_backend({id})"),
            (Driver::Postgres, true) => format!("select pg_terminate_backend({id})"),
            (Driver::MySql, false) => format!("kill query {id}"),
            (Driver::MySql, true) => format!("kill {id}"),
            _ => bail!("not supported"),
        };
        let r = self.raw(sql).await?;
        // Postgres answers false when the process is gone or not ours.
        if r.rows
            .first()
            .and_then(|r| r.first())
            .cloned()
            .flatten()
            .as_deref()
            == Some("false")
        {
            bail!("the server refused: the process is gone, or you lack permission");
        }
        Ok(())
    }

    async fn foreign_keys(&self, obj: &DbObject) -> Result<Vec<ForeignKey>> {
        // One row per (constraint, column position): name, column, ref
        // schema, ref table, ref column, on update, on delete.
        let sql = match self.cfg.driver {
            Driver::Postgres => format!(
                "select c.conname, a.attname, rn.nspname, rc.relname, ra.attname, \
                 c.confupdtype::text, c.confdeltype::text \
                 from pg_constraint c \
                 cross join lateral unnest(c.conkey, c.confkey) with ordinality as k(col, refcol, pos) \
                 join pg_attribute a on a.attrelid = c.conrelid and a.attnum = k.col \
                 join pg_class rc on rc.oid = c.confrelid \
                 join pg_namespace rn on rn.oid = rc.relnamespace \
                 join pg_attribute ra on ra.attrelid = c.confrelid and ra.attnum = k.refcol \
                 where c.contype = 'f' and c.conrelid = to_regclass({}) \
                 order by c.conname, k.pos",
                self.lit(&self.full_name(obj))
            ),
            Driver::MySql => format!(
                "select k.constraint_name, k.column_name, k.referenced_table_schema, \
                 k.referenced_table_name, k.referenced_column_name, r.update_rule, r.delete_rule \
                 from information_schema.key_column_usage k \
                 join information_schema.referential_constraints r \
                   on r.constraint_schema = k.constraint_schema and r.constraint_name = k.constraint_name \
                   and r.table_name = k.table_name \
                 where k.table_schema = database() and k.table_name = {} \
                   and k.referenced_table_name is not null \
                 order by k.constraint_name, k.ordinal_position",
                self.lit(&obj.name)
            ),
            _ => format!(
                "select 'fk_' || id, \"from\", null, \"table\", \"to\", on_update, on_delete \
                 from pragma_foreign_key_list({}) order by id, seq",
                self.lit(&obj.name)
            ),
        };
        let rows = self.raw(sql).await?.rows;
        let mut out: Vec<ForeignKey> = vec![];
        for r in rows {
            let get = |i: usize| r.get(i).cloned().flatten().unwrap_or_default();
            if out.last().is_none_or(|f| f.name != get(0)) {
                out.push(ForeignKey {
                    name: get(0),
                    ref_schema: r.get(2).cloned().flatten(),
                    ref_table: get(3),
                    on_update: fk_action(&get(5)),
                    on_delete: fk_action(&get(6)),
                    ..Default::default()
                });
            }
            let fk = out.last_mut().unwrap();
            fk.columns.push(get(1));
            fk.ref_columns.push(get(4));
        }
        // SQLite: a reference without columns means the parent's primary key.
        for fk in &mut out {
            if fk.ref_columns.iter().any(|c| c.is_empty()) {
                let pk = self
                    .raw(format!(
                        "select name from pragma_table_info({}) where pk > 0 order by pk",
                        self.lit(&fk.ref_table)
                    ))
                    .await?;
                fk.ref_columns = pk.rows.into_iter().filter_map(|r| r[0].clone()).collect();
            }
        }
        Ok(out)
    }

    async fn ddl(&self, obj: &DbObject, columns: &[ColumnInfo]) -> Result<String> {
        let first = |r: QueryResult, col: usize| {
            r.rows
                .into_iter()
                .filter_map(|r| r.get(col).cloned().flatten())
                .collect::<Vec<_>>()
        };
        Ok(match self.cfg.driver {
            Driver::MySql => {
                let r = self
                    .raw(format!("show create table {}", self.q(&obj.name)))
                    .await?;
                format!("{};\n", first(r, 1).join("\n"))
            }
            Driver::Sqlite => {
                let r = self
                    .raw(format!(
                        "select sql from sqlite_master where tbl_name = {} and sql is not null \
                         order by type = 'table' desc, name",
                        self.lit(&obj.name)
                    ))
                    .await?;
                first(r, 0).iter().map(|s| format!("{s};\n")).collect()
            }
            _ => {
                // Postgres has no SHOW CREATE TABLE: rebuild it from the
                // catalog (columns, constraints, then other indexes).
                let name = self.full_name(obj);
                if obj.kind == ObjectKind::View {
                    let r = self
                        .raw(format!(
                            "select pg_get_viewdef(to_regclass({}), true)",
                            self.lit(&name)
                        ))
                        .await?;
                    return Ok(format!(
                        "CREATE VIEW {name} AS\n{}\n",
                        first(r, 0).join("").trim_end()
                    ));
                }
                let mut lines: Vec<String> = columns
                    .iter()
                    .map(|c| {
                        let mut l = format!("  {} {}", pg_ident(&c.name), c.data_type);
                        if let Some(d) = &c.default {
                            l.push_str(&format!(" DEFAULT {d}"));
                        }
                        if !c.nullable {
                            l.push_str(" NOT NULL");
                        }
                        l
                    })
                    .collect();
                let cons = self
                    .raw(format!(
                        "select conname, pg_get_constraintdef(oid, true) from pg_constraint \
                         where conrelid = to_regclass({}) order by contype = 'p' desc, conname",
                        self.lit(&name)
                    ))
                    .await?;
                for r in cons.rows {
                    if let (Some(n), Some(d)) = (&r[0], &r[1]) {
                        lines.push(format!("  CONSTRAINT {} {d}", pg_ident(n)));
                    }
                }
                let mut out = format!("CREATE TABLE {name} (\n{}\n);\n", lines.join(",\n"));
                let idx = self
                    .raw(format!(
                        "select pg_get_indexdef(i.indexrelid) from pg_index i \
                         where i.indrelid = to_regclass({}) and not exists \
                         (select 1 from pg_constraint c where c.conindid = i.indexrelid) order by 1",
                        self.lit(&name)
                    ))
                    .await?;
                for d in first(idx, 0) {
                    out.push_str(&format!("{d};\n"));
                }
                out
            }
        })
    }

    fn where_clause(&self, q: &TableQuery) -> String {
        let parts: Vec<String> = q
            .filters
            .iter()
            .filter(|f| !f.column.is_empty())
            .map(|f| {
                let c = self.q(&f.column);
                let text_c = match self.cfg.driver {
                    Driver::Postgres => format!("{c}::text"),
                    _ => c.clone(),
                };
                let like = match self.cfg.driver {
                    Driver::Postgres => "ilike",
                    _ => "like",
                };
                let v = self.lit(&f.value);
                let pat = self.lit(&format!("%{}%", f.value));
                match f.op {
                    FilterOp::Eq => format!("{c} = {v}"),
                    FilterOp::Ne => format!("{c} <> {v}"),
                    FilterOp::Lt => format!("{c} < {v}"),
                    FilterOp::Gt => format!("{c} > {v}"),
                    FilterOp::Le => format!("{c} <= {v}"),
                    FilterOp::Ge => format!("{c} >= {v}"),
                    FilterOp::Contains => format!("{text_c} {like} {pat}"),
                    FilterOp::NotContains => format!("{text_c} not {like} {pat}"),
                    FilterOp::IsNull => format!("{c} is null"),
                    FilterOp::IsNotNull => format!("{c} is not null"),
                }
            })
            .collect();
        if parts.is_empty() {
            String::new()
        } else {
            format!(" where {}", parts.join(" and "))
        }
    }

    pub async fn fetch_table(&self, q: &TableQuery) -> Result<(QueryResult, Option<u64>)> {
        let table = self.full_name(&q.object);
        let wh = self.where_clause(q);
        let order = q
            .order
            .as_ref()
            .map(|(c, asc)| {
                format!(
                    " order by {} {}",
                    self.q(c),
                    if *asc { "asc" } else { "desc" }
                )
            })
            .unwrap_or_default();
        let sql = format!(
            "select * from {table}{wh}{order} limit {} offset {}",
            q.limit, q.offset
        );
        let res = self.raw(sql).await?;
        let total = self
            .raw(format!("select count(*) from {table}{wh}"))
            .await
            .ok()
            .and_then(|r| r.rows.first().and_then(|r| r[0].as_ref()?.parse().ok()));
        Ok((res, total))
    }

    pub async fn execute(&self, text: &str, cancel: &Cancel) -> Result<Vec<QueryResult>> {
        let stmts = split_statements(text, self.cfg.driver == Driver::MySql);
        Ok(match &self.pool {
            Pool::Pg(p) => run_batch!(
                p,
                stmts,
                pg_row,
                cancel,
                Some(("select pg_backend_pid()", "select pg_cancel_backend({})"))
            ),
            Pool::My(p) => run_batch!(
                p,
                stmts,
                my_row,
                cancel,
                Some(("select connection_id()", "kill query {}"))
            ),
            // SQLite runs in-process: abandoning the batch is enough.
            Pool::Lite(p) => run_batch!(p, stmts, lite_row, cancel, None),
        })
    }

    /// Builds (sql, params, must_affect_exactly_one_row) per change. With
    /// `inline`, values are written as literals (for the preview) instead of
    /// bound parameters.
    fn statements(
        &self,
        obj: &DbObject,
        columns: &[ColumnInfo],
        changes: &[Change],
        inline: bool,
    ) -> Vec<(String, Vec<Option<String>>, bool)> {
        let table = self.full_name(obj);
        let pg = self.cfg.driver == Driver::Postgres;
        let ty = |col: &str| {
            columns
                .iter()
                .find(|c| c.name == col)
                .map(|c| c.data_type.clone())
        };
        let mut out = Vec::new();
        for ch in changes {
            let mut params: Vec<Option<String>> = Vec::new();
            // Placeholder (or literal) for a value of column `col`.
            let mut val = |col: &str, v: &Option<String>| -> String {
                if inline {
                    return sql_literal(v);
                }
                params.push(v.clone());
                let n = params.len();
                match (pg, ty(col)) {
                    (true, Some(t)) => format!("cast(${n}::text as {t})"),
                    (true, None) => format!("${n}"),
                    _ => "?".into(),
                }
            };
            let (sql, one) = match ch {
                Change::Update { key, set } => {
                    let sets: Vec<String> = set
                        .iter()
                        .map(|(c, v)| format!("{} = {}", self.q(c), val(c, v)))
                        .collect();
                    let wh = self.key_where(key, &mut val);
                    (
                        format!("update {table} set {} where {wh}", sets.join(", ")),
                        true,
                    )
                }
                Change::Insert { values } => {
                    let values: Vec<_> = values.iter().filter(|(_, v)| v.is_some()).collect();
                    if values.is_empty() {
                        let sql = match self.cfg.driver {
                            Driver::MySql => format!("insert into {table} () values ()"),
                            _ => format!("insert into {table} default values"),
                        };
                        (sql, true)
                    } else {
                        let cols: Vec<_> = values.iter().map(|(c, _)| self.q(c)).collect();
                        let vals: Vec<_> = values.iter().map(|(c, v)| val(c, v)).collect();
                        (
                            format!(
                                "insert into {table} ({}) values ({})",
                                cols.join(", "),
                                vals.join(", ")
                            ),
                            true,
                        )
                    }
                }
                Change::Delete { key } => {
                    let wh = self.key_where(key, &mut val);
                    (format!("delete from {table} where {wh}"), true)
                }
            };
            out.push((sql, params, one));
        }
        out
    }

    fn key_where(
        &self,
        key: &[(String, Option<String>)],
        val: &mut impl FnMut(&str, &Option<String>) -> String,
    ) -> String {
        key.iter()
            .map(|(c, v)| match v {
                None => format!("{} is null", self.q(c)),
                Some(_) => format!("{} = {}", self.q(c), val(c, v)),
            })
            .collect::<Vec<_>>()
            .join(" and ")
    }

    pub fn preview(&self, obj: &DbObject, columns: &[ColumnInfo], changes: &[Change]) -> String {
        self.statements(obj, columns, changes, true)
            .into_iter()
            .map(|(sql, _, _)| sql + ";")
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub async fn commit(
        &self,
        obj: &DbObject,
        columns: &[ColumnInfo],
        changes: &[Change],
    ) -> Result<()> {
        let stmts = self.statements(obj, columns, changes, false);
        match &self.pool {
            Pool::Pg(p) => run_tx!(p, stmts),
            Pool::My(p) => run_tx!(p, stmts),
            Pool::Lite(p) => run_tx!(p, stmts),
        }
        Ok(())
    }

    pub async fn drop_object(&self, obj: &DbObject) -> Result<()> {
        let kw = if obj.kind == ObjectKind::View {
            "view"
        } else {
            "table"
        };
        self.raw(format!("drop {kw} {}", self.full_name(obj)))
            .await?;
        Ok(())
    }

    pub async fn create_database(&self, name: &str) -> Result<()> {
        match self.cfg.driver {
            Driver::Postgres | Driver::MySql => {
                self.raw(format!("create database {}", self.q(name)))
                    .await?;
                Ok(())
            }
            _ => bail!("SQLite has one database per file"),
        }
    }

    pub async fn drop_database(&self, name: &str) -> Result<()> {
        if name == self.database {
            bail!("switch to another database before dropping this one");
        }
        match self.cfg.driver {
            Driver::Postgres | Driver::MySql => {
                self.raw(format!("drop database {}", self.q(name))).await?;
                Ok(())
            }
            _ => bail!("SQLite has one database per file"),
        }
    }

    pub async fn rename_object(&self, obj: &DbObject, new_name: &str) -> Result<()> {
        let new = self.q(new_name);
        let sql = match (self.cfg.driver, obj.kind) {
            (Driver::MySql, _) => format!("rename table {} to {new}", self.full_name(obj)),
            (Driver::Postgres, ObjectKind::View) => {
                format!("alter view {} rename to {new}", self.full_name(obj))
            }
            (_, ObjectKind::View) => bail!("SQLite can't rename a view; recreate it instead"),
            _ => format!("alter table {} rename to {new}", self.full_name(obj)),
        };
        self.raw(sql).await?;
        Ok(())
    }

    /// Copies a table's structure (and with `data`, its rows) to `new_name`
    /// in the same schema, in one transaction where the server allows it.
    pub async fn duplicate_table(&self, obj: &DbObject, new_name: &str, data: bool) -> Result<()> {
        let src = self.full_name(obj);
        let dst = self.full_name(&DbObject {
            name: new_name.to_string(),
            ..obj.clone()
        });
        let script = match self.cfg.driver {
            Driver::Postgres => {
                let mut s = format!("begin; create table {dst} (like {src} including all);");
                if data {
                    s.push_str(&format!(" insert into {dst} select * from {src};"));
                }
                s.push_str(" commit;");
                s
            }
            Driver::MySql => {
                let mut s = format!("create table {dst} like {src};");
                if data {
                    s.push_str(&format!(" insert into {dst} select * from {src};"));
                }
                s
            }
            _ => {
                // Reuse the original CREATE statement under the new name, so
                // keys and constraints come along.
                let r = self
                    .raw(format!(
                        "select sql from sqlite_master where type = 'table' and name = {}",
                        self.lit(&obj.name)
                    ))
                    .await?;
                let ddl = r
                    .rows
                    .first()
                    .and_then(|r| r[0].clone())
                    .context("table not found")?;
                let open = ddl.find('(').context("unexpected CREATE statement")?;
                let mut s = format!("begin; create table {dst} {};", &ddl[open..]);
                if data {
                    s.push_str(&format!(" insert into {dst} select * from {src};"));
                }
                s.push_str(" commit;");
                s
            }
        };
        self.execute(&script, &Default::default()).await?;
        Ok(())
    }

    /// VACUUM ANALYZE / OPTIMIZE TABLE: reclaims space, refreshes statistics.
    pub async fn maintain(&self, obj: &DbObject) -> Result<String> {
        let t = self.full_name(obj);
        let (sql, done) = match self.cfg.driver {
            Driver::Postgres => (format!("vacuum (analyze) {t}"), "Vacuumed and analyzed"),
            Driver::MySql => (format!("optimize table {t}"), "Optimized"),
            _ => (
                "vacuum; analyze".to_string(),
                "Vacuumed and analyzed the database",
            ),
        };
        self.execute(&sql, &Default::default()).await?;
        Ok(done.to_string())
    }

    pub async fn truncate(&self, obj: &DbObject) -> Result<()> {
        let sql = match self.cfg.driver {
            Driver::Sqlite => format!("delete from {}", self.full_name(obj)),
            _ => format!("truncate table {}", self.full_name(obj)),
        };
        self.raw(sql).await?;
        Ok(())
    }

    pub async fn close(&self) {
        match &self.pool {
            Pool::Pg(p) => p.close().await,
            Pool::My(p) => p.close().await,
            Pool::Lite(p) => p.close().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_option_checks_certificates() {
        let mut cfg = ConnectionConfig::new(Driver::Postgres);
        assert!(matches!(pg_ssl_mode(&cfg), PgSslMode::Prefer));
        cfg.ssl = true;
        assert!(matches!(pg_ssl_mode(&cfg), PgSslMode::Require));
        assert!(matches!(my_ssl_mode(&cfg), MySqlSslMode::Required));
        cfg.ssl_verify = true;
        assert!(matches!(pg_ssl_mode(&cfg), PgSslMode::VerifyFull));
        assert!(matches!(my_ssl_mode(&cfg), MySqlSslMode::VerifyIdentity));
    }

    #[test]
    fn check_in_lists_in_each_dialect() {
        let sqlite = "CREATE TABLE orders (id integer primary key, \
            status text check (status in ('pending','paid', 'it''s')), n int CHECK (n > 0))";
        assert_eq!(
            check_in_lists(sqlite),
            [(
                "status".to_string(),
                vec!["pending".into(), "paid".into(), "it's".into()]
            )]
        );
        let mysql = "CONSTRAINT `orders_chk_1` CHECK ((`status` in \
            (_utf8mb4'pending',_latin1'it\\'s paid')))";
        assert_eq!(
            check_in_lists(mysql),
            [(
                "status".to_string(),
                vec!["pending".into(), "it's paid".into()]
            )]
        );
        let pg = "CONSTRAINT orders_status_check CHECK ((status = ANY \
            (ARRAY['pending'::text, 'paid'::text])))";
        assert_eq!(
            check_in_lists(pg),
            [("status".to_string(), vec!["pending".into(), "paid".into()])]
        );
        let pg_varchar = "CHECK (((state)::text = ANY ((ARRAY['a'::character varying, \
            'b'::character varying])::text[])))";
        assert_eq!(
            check_in_lists(pg_varchar),
            [("state".to_string(), vec!["a".into(), "b".into()])]
        );
        // Anything more than a plain list isn't an enum.
        assert!(check_in_lists("CHECK (status in ('a') or x > 1)").is_empty());
        assert!(check_in_lists("CHECK (lower(status) in ('a'))").is_empty());
        assert!(check_in_lists("CHECK (n in (1, 2))").is_empty());
        assert!(check_in_lists("checked boolean").is_empty());
    }

    #[test]
    fn reads_mysql_enum_values() {
        assert_eq!(mysql_enum("enum('a','it''s','b,c')"), ["a", "it's", "b,c"]);
        assert_eq!(mysql_enum("enum('')"), [""]);
        assert!(mysql_enum("varchar(20)").is_empty());
        assert!(mysql_enum("set('a','b')").is_empty());
    }

    fn after(stmts: &[&str]) -> SessionEffects {
        let mut e = SessionEffects::default();
        for s in stmts {
            e.observe(s);
        }
        e
    }

    #[test]
    fn tracks_transactions_and_session_changes() {
        assert!(after(&["BEGIN", "delete from t"]).tx_open);
        assert!(!after(&["begin", "delete from t", "ROLLBACK"]).tx_open);
        assert!(!after(&["start transaction", "update t set a=1", "commit"]).tx_open);
        assert!(after(&["begin", "savepoint s", "rollback to savepoint s"]).tx_open);
        assert!(after(&["SET search_path TO x"]).dirty);
        assert!(after(&["use other_db"]).dirty);
        assert!(after(&["create temp table x (a int)"]).dirty);
        assert!(after(&["select set_config('a.b', '1', false)"]).dirty);
        assert!(after(&["pragma foreign_keys = on"]).dirty);
        assert_eq!(
            after(&["select 1", "pragma table_info(t)", "update t set a = 1"]),
            SessionEffects::default()
        );
    }
}
