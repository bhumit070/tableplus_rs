//! Query history: every batch run from a query tab, kept per connection in
//! `history.db` (SQLite) next to the other config files. Local only.

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Row};
use tokio::sync::OnceCell;

/// Entries kept per connection; older ones are pruned.
const KEEP: i64 = 5000;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub conn_id: String,
    pub database: String,
    pub sql: String,
    /// Unix seconds.
    pub ran_at: i64,
    pub elapsed_ms: i64,
    /// Rows returned or affected by the last statement.
    pub rows: Option<i64>,
    pub error: Option<String>,
}

static POOL: OnceCell<SqlitePool> = OnceCell::const_new();

async fn pool() -> Result<&'static SqlitePool> {
    POOL.get_or_try_init(|| async {
        let dir = crate::store::config_dir();
        std::fs::create_dir_all(&dir)?;
        let o = SqliteConnectOptions::new()
            .filename(dir.join("history.db"))
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(o)
            .await?;
        // Schema version, so later changes can migrate old files.
        let version: i64 = sqlx::query_scalar("pragma user_version")
            .fetch_one(&pool)
            .await?;
        if version > 1 {
            anyhow::bail!(
                "history.db was written by a newer Tabula; history is off until you update"
            );
        }
        sqlx::raw_sql(
            "create table if not exists history (
                id integer primary key,
                conn_id text not null,
                database text not null,
                sql text not null,
                ran_at integer not null,
                elapsed_ms integer not null,
                rows integer,
                error text
            );
            create index if not exists history_conn on history (conn_id, ran_at);
            pragma user_version = 1;",
        )
        .execute(&pool)
        .await?;
        Ok(pool)
    })
    .await
}

pub async fn record(e: Entry) -> Result<()> {
    let p = pool().await?;
    sqlx::query(
        "insert into history (conn_id, database, sql, ran_at, elapsed_ms, rows, error)
         values (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&e.conn_id)
    .bind(&e.database)
    .bind(&e.sql)
    .bind(e.ran_at)
    .bind(e.elapsed_ms)
    .bind(e.rows)
    .bind(&e.error)
    .execute(p)
    .await?;
    sqlx::query(AssertSqlSafe(format!(
        "delete from history where conn_id = ? and id not in
         (select id from history where conn_id = ? order by id desc limit {KEEP})"
    )))
    .bind(&e.conn_id)
    .bind(&e.conn_id)
    .execute(p)
    .await?;
    Ok(())
}

/// Newest first, one entry per distinct SQL text (its latest run).
pub async fn recent(conn_id: &str, limit: i64) -> Result<Vec<Entry>> {
    let rows = sqlx::query(
        "select conn_id, database, sql, ran_at, elapsed_ms, rows, error from history
         where id in (select max(id) from history where conn_id = ? group by sql)
         order by id desc limit ?",
    )
    .bind(conn_id)
    .bind(limit)
    .fetch_all(pool().await?)
    .await?;
    Ok(rows
        .iter()
        .map(|r| Entry {
            conn_id: r.get(0),
            database: r.get(1),
            sql: r.get(2),
            ran_at: r.get(3),
            elapsed_ms: r.get(4),
            rows: r.get(5),
            error: r.get(6),
        })
        .collect())
}

pub async fn clear(conn_id: &str) -> Result<()> {
    sqlx::query("delete from history where conn_id = ?")
        .bind(conn_id)
        .execute(pool().await?)
        .await?;
    Ok(())
}

/// "just now", "5 min ago", "3 h ago", "2 d ago".
pub fn ago(ran_at: i64, now: i64) -> String {
    let s = (now - ran_at).max(0);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86400 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86400),
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(conn: &str, sql: &str, at: i64) -> Entry {
        Entry {
            conn_id: conn.into(),
            database: "app".into(),
            sql: sql.into(),
            ran_at: at,
            elapsed_ms: 3,
            rows: Some(1),
            error: None,
        }
    }

    #[test]
    fn keeps_latest_run_per_query_per_connection() {
        // On the app's runtime, which owns the shared pool.
        let conn = format!("hist-{}", uuid::Uuid::new_v4());
        let got = futures::executor::block_on(crate::runtime::spawn(async move {
            record(entry(&conn, "select 1", 10)).await?;
            record(entry(&conn, "select 2", 20)).await?;
            record(entry(&conn, "select 1", 30)).await?;
            record(entry("someone-else", "select 3", 40)).await?;
            let got = recent(&conn, 10).await?;
            clear(&conn).await?;
            Ok((got, recent(&conn, 10).await?))
        }))
        .unwrap();
        assert_eq!(
            got.0
                .iter()
                .map(|e| (e.sql.as_str(), e.ran_at))
                .collect::<Vec<_>>(),
            [("select 1", 30), ("select 2", 20)]
        );
        assert!(got.1.is_empty());
    }

    #[test]
    fn ago_is_coarse() {
        assert_eq!(ago(100, 110), "just now");
        assert_eq!(ago(0, 300), "5 min ago");
        assert_eq!(ago(0, 7200), "2 h ago");
        assert_eq!(ago(0, 3 * 86400), "3 d ago");
    }
}
