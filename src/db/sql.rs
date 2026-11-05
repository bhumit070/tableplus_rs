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
    AssertSqlSafe, Column, Either, Executor, Row, SqlSafeStr, Statement, TypeInfo, ValueRef,
};

use super::split::split_statements;
use super::{
    Change, ColumnInfo, ConnectionConfig, DbObject, Driver, FilterOp, ObjectKind, QueryResult,
    Structure, TableQuery, sql_literal,
};

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

fn pg_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

// ---- row decoding ---------------------------------------------------------

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
        let mut stream = sqlx::raw_sql(AssertSqlSafe(sql.clone())).fetch_many($pool);
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

/// Applies a list of (sql, params) in one transaction.
macro_rules! run_tx {
    ($pool:expr, $stmts:expr) => {{
        let mut tx = $pool.begin().await?;
        for (sql, params, expect_one) in $stmts {
            let mut q = sqlx::query(AssertSqlSafe(sql.clone()));
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
        Ok(match cfg.driver {
            Driver::Postgres => {
                let mut o = PgConnectOptions::new()
                    .host(&cfg.host)
                    .port(cfg.port)
                    .username(&cfg.user)
                    .password(&cfg.password)
                    .application_name("tabula")
                    .ssl_mode(if cfg.ssl {
                        PgSslMode::Require
                    } else {
                        PgSslMode::Prefer
                    });
                if !database.is_empty() {
                    o = o.database(database);
                }
                let mut pool = PgPoolOptions::new()
                    .max_connections(4)
                    .acquire_timeout(timeout);
                if let Some(schema) = schema {
                    // Every pooled connection resolves unqualified names in
                    // the chosen schema first; `public` keeps extensions working.
                    let sql = format!("set search_path to {}, public", pg_ident(schema));
                    pool = pool.after_connect(move |conn, _| {
                        let sql = sql.clone();
                        Box::pin(async move {
                            sqlx::raw_sql(AssertSqlSafe(sql))
                                .execute(&mut *conn)
                                .await
                                .map(|_| ())
                        })
                    });
                }
                Pool::Pg(pool.connect_with(o).await?)
            }
            Driver::MySql => {
                let mut o = MySqlConnectOptions::new()
                    .host(&cfg.host)
                    .port(cfg.port)
                    .username(&cfg.user)
                    .password(&cfg.password)
                    .ssl_mode(if cfg.ssl {
                        MySqlSslMode::Required
                    } else {
                        MySqlSslMode::Preferred
                    });
                if !database.is_empty() {
                    o = o.database(database);
                }
                Pool::My(
                    MySqlPoolOptions::new()
                        .max_connections(4)
                        .acquire_timeout(timeout)
                        .connect_with(o)
                        .await?,
                )
            }
            Driver::Sqlite => {
                if cfg.file.is_empty() {
                    bail!("choose a SQLite database file");
                }
                let o = SqliteConnectOptions::new()
                    .filename(&cfg.file)
                    .create_if_missing(true);
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
                       and a.attnum = any(i.indkey)))::text \
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
        let columns = self
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
            })
            .collect();
        let indexes = self.raw(idx_sql).await.unwrap_or_default();
        Ok(Structure { columns, indexes })
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

    pub async fn execute(&self, text: &str) -> Result<Vec<QueryResult>> {
        let mut out = Vec::new();
        for stmt in split_statements(text) {
            let start = Instant::now();
            let mut r = self.raw(stmt).await?;
            r.elapsed_ms = start.elapsed().as_millis();
            out.push(r);
        }
        Ok(out)
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
