//! Microsoft SQL Server (and Azure SQL) via tiberius, behind the same API
//! as `sql::SqlConn`.
//!
//! Editor text runs as T-SQL batches (`split::split_mssql`), every value is
//! turned into text here so the grid needs no per-type code, and writes use
//! bound `@Pn` parameters. tiberius has no pool, so a small one lives here.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Error, Result, anyhow, bail};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use futures::TryStreamExt;
use tiberius::error::Error as TdsError;
use tiberius::{
    AuthMethod, Client, ColumnData, Config, EncryptionLevel, FromSql, Query, QueryItem,
};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio::{select, spawn as tokio_spawn, time::timeout};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use super::binary::{encode, hex_digits, is_binary_type};
use super::cancel::{CANCELLED, Cancel};
use super::safety::is_read_only;
use super::split::split_mssql;
use super::sql::{CONNECT_TIMEOUT, SessionEffects, check_in_lists, unwrap_parens};
use super::{
    BatchFailed, Change, ColumnInfo, ConnectionConfig, DatabaseStats, DbObject, Driver, Filter,
    FilterOp, ForeignKey, ObjectKind, QueryResult, SafeMode, Structure, TableQuery,
};

type TdsClient = Client<Compat<TcpStream>>;

/// Connections per pool, like the sqlx pools.
const MAX_CONNECTIONS: usize = 4;
/// How long a request waits for a free pooled connection.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
/// A connection idle longer than this is pinged before it's reused, so a
/// server that dropped it doesn't fail the next request.
const PING_AFTER: Duration = Duration::from_secs(60);

/// What every new connection sets: the SSMS defaults (QUOTED_IDENTIFIER
/// lets the other drivers' "quoted" names work; ANSI settings are needed
/// for indexed views and computed-column indexes), and no 4 KB cut-off on
/// long text.
const SESSION_SETUP: &str = "set quoted_identifier on; set ansi_nulls on; set ansi_padding on; \
     set ansi_warnings on; set concat_null_yields_null on; set arithabort on; \
     set textsize 2147483647";

/// The column type as written in DDL: `nvarchar(40)`, `decimal(10,2)`…
/// Needs `c` (sys.columns) and `t` (sys.types).
const TYPE_SQL: &str = "case \
     when t.name in ('varchar', 'char', 'varbinary', 'binary') then t.name + '(' + \
       case when c.max_length = -1 then 'max' else cast(c.max_length as varchar(10)) end + ')' \
     when t.name in ('nvarchar', 'nchar') then t.name + '(' + \
       case when c.max_length = -1 then 'max' else cast(c.max_length / 2 as varchar(10)) end + ')' \
     when t.name in ('decimal', 'numeric') then t.name + '(' + cast(c.precision as varchar(10)) + \
       ',' + cast(c.scale as varchar(10)) + ')' \
     when t.name in ('datetime2', 'time', 'datetimeoffset') then t.name + '(' + \
       cast(c.scale as varchar(10)) + ')' \
     else t.name end";

/// `[name]`, with `]` doubled.
fn ident(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

/// A Unicode string literal: `N'it''s'`.
fn lit(text: &str) -> String {
    format!("N'{}'", text.replace('\'', "''"))
}

/// Server errors read like SSMS's ("Invalid object name 'x'."), with the
/// error number and line; anything else (I/O, TLS) is kept as it is, so
/// `explain_lost` still recognises a dropped connection.
fn tidy(e: TdsError) -> Error {
    match e {
        TdsError::Server(token) => anyhow!(
            "{} (error {}, line {})",
            token.message(),
            token.code(),
            token.line()
        ),
        other => Error::new(other),
    }
}

// ---- connections ----------------------------------------------------------

async fn open_client(cfg: &ConnectionConfig, database: &str) -> Result<TdsClient> {
    let mut config = Config::new();
    config.host(&cfg.host);
    config.port(cfg.port);
    config.authentication(AuthMethod::sql_server(&cfg.user, &cfg.password));
    config.application_name("tabula");
    if !database.is_empty() {
        config.database(database);
    }
    // Unchecked SSL still encrypts when the server can (like Postgres'
    // "prefer"); checked, it must.
    config.encryption(if cfg.ssl {
        EncryptionLevel::Required
    } else {
        EncryptionLevel::On
    });
    // The login is encrypted either way; most servers have a self-signed
    // certificate, so it's only checked when asked to (with SSL on).
    if !(cfg.ssl && cfg.ssl_verify) {
        config.trust_cert();
    }
    let at = format!("{}:{}", cfg.host, cfg.port);
    let connect = async move {
        // Azure SQL may redirect the login to another node, once.
        for _ in 0..2 {
            let tcp = TcpStream::connect(config.get_addr()).await?;
            tcp.set_nodelay(true)?;
            match Client::connect(config.clone(), tcp.compat_write()).await {
                Err(TdsError::Routing { host, port }) => {
                    config.host(host);
                    config.port(port);
                }
                Err(e) => return Err(tidy(e)),
                Ok(client) => return Ok(client),
            }
        }
        bail!("the server redirected the connection more than once")
    };
    let mut client = match timeout(CONNECT_TIMEOUT, connect).await {
        Err(_) => bail!("no answer from {at} within {} s", CONNECT_TIMEOUT.as_secs()),
        Ok(result) => result.with_context(|| format!("couldn't connect to {at}"))?,
    };
    run_one(&mut client, SESSION_SETUP).await?;
    Ok(client)
}

/// A handful of open connections to one database.
struct Pool {
    cfg: ConnectionConfig,
    database: String,
    idle: Mutex<Vec<(TdsClient, Instant)>>,
    permits: Arc<Semaphore>,
}

/// A connection taken from the pool. It goes back only through `release`,
/// after a request that finished cleanly; dropped any other way (an error,
/// a timeout, a cancel) it's closed, so no half-read result or leftover
/// session state reaches the next request.
struct Pooled {
    client: Option<TdsClient>,
    pool: Arc<Pool>,
    _permit: OwnedSemaphorePermit,
}

impl Pooled {
    fn client(&mut self) -> &mut TdsClient {
        self.client.as_mut().expect("present until released")
    }

    fn release(mut self) {
        if let Some(client) = self.client.take() {
            self.pool
                .idle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((client, Instant::now()));
        }
    }
}

impl Pool {
    fn new(cfg: &ConnectionConfig, database: &str) -> Arc<Pool> {
        Arc::new(Pool {
            cfg: cfg.clone(),
            database: database.to_string(),
            idle: Mutex::new(vec![]),
            permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        })
    }

    async fn get(self: &Arc<Self>) -> Result<Pooled> {
        let permit = timeout(ACQUIRE_TIMEOUT, self.permits.clone().acquire_owned())
            .await
            .map_err(|_| {
                anyhow!(
                    "all {MAX_CONNECTIONS} connections are busy; stop a running query or try again"
                )
            })?
            .context("the connection was closed")?;
        let mut client = None;
        loop {
            let idle = self.idle.lock().unwrap_or_else(|e| e.into_inner()).pop();
            let Some((mut candidate, since)) = idle else {
                break;
            };
            if since.elapsed() < PING_AFTER || run_one(&mut candidate, "select 1").await.is_ok() {
                client = Some(candidate);
                break;
            }
        }
        let client = match client {
            Some(client) => client,
            // Boxed: the TLS handshake makes this future large, and every
            // request embeds it.
            None => Box::pin(open_client(&self.cfg, &self.database)).await?,
        };
        Ok(Pooled {
            client: Some(client),
            pool: self.clone(),
            _permit: permit,
        })
    }

    fn close(&self) {
        self.idle.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

// ---- values as text ---------------------------------------------------------

/// Fractional seconds as SQL Server writes them back: at most 7 digits,
/// trailing zeros dropped, nothing for whole seconds.
fn fraction(nanos: u32) -> String {
    let digits = format!("{:07}", nanos / 100);
    let digits = digits.trim_end_matches('0');
    if digits.is_empty() {
        String::new()
    } else {
        format!(".{digits}")
    }
}

fn date_time_text(value: NaiveDateTime) -> String {
    format!(
        "{}{}",
        value.format("%Y-%m-%d %H:%M:%S"),
        fraction(value.nanosecond())
    )
}

/// `datetime` counts days from 1900 and 1/300 s ticks, shown (as SQL
/// Server does) rounded to the millisecond: .997, not .9966666.
fn legacy_date_time(value: tiberius::time::DateTime) -> Option<NaiveDateTime> {
    let day = NaiveDate::from_ymd_opt(1900, 1, 1)?
        .checked_add_signed(chrono::Duration::days(value.days().into()))?;
    // ticks × 10/3 ms, to the nearest.
    let millis = (i64::from(value.seconds_fragments()) * 10 + 1) / 3;
    day.and_hms_opt(0, 0, 0)?
        .checked_add_signed(chrono::Duration::milliseconds(millis))
}

fn time_text(value: NaiveTime) -> String {
    format!(
        "{}{}",
        value.format("%H:%M:%S"),
        fraction(value.nanosecond())
    )
}

/// One value as the grid shows it: bits as 1 / 0 (as SSMS does), binary as
/// `0x…`, dates in ISO form so they can be written back unchanged.
fn cell(data: &ColumnData<'static>) -> Option<String> {
    match data {
        ColumnData::U8(value) => value.map(|n| n.to_string()),
        ColumnData::I16(value) => value.map(|n| n.to_string()),
        ColumnData::I32(value) => value.map(|n| n.to_string()),
        ColumnData::I64(value) => value.map(|n| n.to_string()),
        ColumnData::F32(value) => value.map(|n| n.to_string()),
        ColumnData::F64(value) => value.map(|n| n.to_string()),
        ColumnData::Bit(value) => value.map(|b| if b { "1" } else { "0" }.to_string()),
        ColumnData::String(value) => value.as_ref().map(|text| text.to_string()),
        ColumnData::Guid(value) => value.map(|guid| guid.to_string().to_uppercase()),
        ColumnData::Binary(value) => value.as_ref().map(|bytes| encode(Driver::Mssql, bytes)),
        ColumnData::Numeric(value) => value.map(|n| n.to_string()),
        ColumnData::Xml(value) => value.as_ref().map(|xml| xml.to_string()),
        ColumnData::DateTime(value) => value.and_then(legacy_date_time).map(date_time_text),
        ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => NaiveDateTime::from_sql(data)
            .ok()
            .flatten()
            .map(date_time_text),
        ColumnData::Date(_) => NaiveDate::from_sql(data)
            .ok()
            .flatten()
            .map(|date| date.format("%Y-%m-%d").to_string()),
        ColumnData::Time(_) => NaiveTime::from_sql(data).ok().flatten().map(time_text),
        ColumnData::DateTimeOffset(_) => DateTime::<FixedOffset>::from_sql(data)
            .ok()
            .flatten()
            .map(|value| {
                format!(
                    "{} {}",
                    date_time_text(value.naive_local()),
                    value.format("%:z")
                )
            }),
    }
}

fn column_names<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    names
        .map(|name| match name {
            "" => "(No column name)".to_string(),
            name => name.to_string(),
        })
        .collect()
}

/// Runs one batch: one result per result set it returns, or one with the
/// affected row count when it returns none.
async fn run_one(client: &mut TdsClient, sql: &str) -> Result<Vec<QueryResult>> {
    let mut stream = client.simple_query(sql).await.map_err(tidy)?;
    let mut results: Vec<QueryResult> = vec![];
    let fresh = |columns: Vec<String>| QueryResult {
        columns,
        statement: sql.to_string(),
        ..Default::default()
    };
    while let Some(item) = stream.try_next().await.map_err(tidy)? {
        match item {
            QueryItem::Metadata(meta) => {
                results.push(fresh(column_names(
                    meta.columns().iter().map(|column| column.name()),
                )));
            }
            QueryItem::Row(row) => {
                if results.is_empty() {
                    results.push(fresh(column_names(
                        row.columns().iter().map(|column| column.name()),
                    )));
                }
                let values = row.cells().map(|(_, data)| cell(data)).collect();
                if let Some(result) = results.last_mut() {
                    result.rows.push(values);
                }
            }
        }
    }
    drop(stream);
    if results.is_empty() {
        let count = client
            .simple_query("select rowcount_big()")
            .await
            .map_err(tidy)?
            .into_row()
            .await
            .map_err(tidy)?
            .and_then(|row| row.get::<i64, _>(0));
        results.push(QueryResult {
            affected: count.map(|n| n.max(0) as u64),
            ..fresh(vec![])
        });
    }
    Ok(results)
}

/// What a finished editor batch left behind, from its task.
struct BatchOutcome {
    done: Vec<QueryResult>,
    failed: Option<(usize, Error)>,
    rolled_back: bool,
}

/// A table column beyond what `ColumnInfo` holds, for the DDL.
#[derive(Default)]
struct ColumnExtra {
    /// (seed, increment) of an IDENTITY column.
    identity: Option<(String, String)>,
    /// `AS (expression)` of a computed column.
    computed: Option<String>,
    default_name: Option<String>,
    /// rowversion / timestamp: the server fills it in.
    row_version: bool,
}

pub struct MssqlConn {
    cfg: ConnectionConfig,
    pool: Arc<Pool>,
    database: String,
}

impl MssqlConn {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        let pool = Pool::new(cfg, &cfg.database);
        // One connection up front, so a wrong password or host fails here
        // with the server's own reason.
        pool.get().await?.release();
        let mut conn = MssqlConn {
            cfg: cfg.clone(),
            pool,
            database: cfg.database.clone(),
        };
        conn.database = conn.scalar("select db_name()").await?.unwrap_or_default();
        Ok(conn)
    }

    async fn raw_all(&self, sql: &str) -> Result<Vec<QueryResult>> {
        let mut conn = self.pool.get().await?;
        let results = run_one(conn.client(), sql).await?;
        conn.release();
        Ok(results)
    }

    /// The first result of `sql` (internal reads return one).
    async fn raw(&self, sql: &str) -> Result<QueryResult> {
        Ok(self
            .raw_all(sql)
            .await?
            .into_iter()
            .next()
            .unwrap_or_default())
    }

    async fn scalar(&self, sql: &str) -> Result<Option<String>> {
        Ok(self
            .raw(sql)
            .await?
            .rows
            .into_iter()
            .next()
            .and_then(|row| row.into_iter().next().flatten()))
    }

    /// Read-only safe mode. SQL Server has no read-only session to turn on,
    /// so it's enforced here, below the UI's own check: only text that
    /// `safety` reads as reading gets through (`""` = a write).
    fn refuse_writes(&self, text: &str) -> Result<()> {
        if self.cfg.safety() == SafeMode::ReadOnly && !is_read_only(Driver::Mssql, text) {
            bail!("This connection is read-only (Safe mode): only reads can run.");
        }
        Ok(())
    }

    /// `[schema].[name]`; `dbo` when the object has no schema.
    fn full_name(&self, obj: &DbObject) -> String {
        format!(
            "{}.{}",
            ident(obj.schema.as_deref().unwrap_or("dbo")),
            ident(&obj.name)
        )
    }

    /// `object_id(N'[schema].[name]')`.
    fn object_id(&self, obj: &DbObject) -> String {
        format!("object_id({})", lit(&self.full_name(obj)))
    }

    pub fn current_database(&self) -> String {
        self.database.clone()
    }

    pub async fn databases(&self) -> Result<Vec<String>> {
        Ok(self
            .raw("select name from sys.databases where has_dbaccess(name) = 1 order by name")
            .await?
            .rows
            .into_iter()
            .filter_map(|row| row.into_iter().next().flatten())
            .collect())
    }

    /// Row counts the server keeps per partition (no COUNT).
    pub async fn approx_row_counts(&self) -> Result<Vec<(Option<String>, String, u64)>> {
        let sql = "select s.name, t.name, sum(p.rows) from sys.tables t \
             join sys.schemas s on s.schema_id = t.schema_id \
             join sys.partitions p on p.object_id = t.object_id and p.index_id in (0, 1) \
             group by s.name, t.name";
        Ok(self
            .raw(sql)
            .await?
            .rows
            .into_iter()
            .filter_map(|row| {
                let rows = row.get(2)?.as_deref()?.parse::<u64>().ok()?;
                Some((row.first()?.clone(), row.get(1)?.clone()?, rows))
            })
            .collect())
    }

    /// Database sizes from their files. Reading other databases' files
    /// needs VIEW ANY DEFINITION: without it there are no numbers.
    pub async fn database_stats(&self) -> Result<Vec<DatabaseStats>> {
        let sql = "select d.name, sum(cast(f.size as bigint)) * 8192 from sys.databases d \
             join sys.master_files f on f.database_id = d.database_id group by d.name";
        let Ok(result) = self.raw(sql).await else {
            return Ok(vec![]);
        };
        Ok(result
            .rows
            .into_iter()
            .filter_map(|row| {
                Some(DatabaseStats {
                    name: row.first()?.clone()?,
                    size_bytes: row.get(1)?.as_deref()?.parse().ok(),
                    objects: None,
                })
            })
            .collect())
    }

    pub async fn use_database(&mut self, name: &str) -> Result<()> {
        let pool = Pool::new(&self.cfg, name);
        pool.get().await?.release();
        self.pool.close();
        self.pool = pool;
        self.database = name.to_string();
        Ok(())
    }

    /// (schema, table, column) for every column, for autocomplete.
    pub async fn all_columns(&self) -> Result<Vec<(String, String, String)>> {
        let sql = "select top 50000 table_schema, table_name, column_name \
             from information_schema.columns order by table_name, ordinal_position";
        Ok(self
            .raw(sql)
            .await?
            .rows
            .into_iter()
            .filter_map(|row| Some((row[0].clone()?, row[1].clone()?, row[2].clone()?)))
            .collect())
    }

    pub async fn list_objects(&self) -> Result<Vec<DbObject>> {
        let sql = "select s.name, o.name, o.type from sys.objects o \
             join sys.schemas s on s.schema_id = o.schema_id \
             where o.type in ('U', 'V', 'P', 'FN', 'IF', 'TF') and o.is_ms_shipped = 0 \
             order by s.name, o.name";
        Ok(self
            .raw(sql)
            .await?
            .rows
            .into_iter()
            .filter_map(|row| {
                let kind = match row.get(2)?.as_deref()?.trim() {
                    "U" => ObjectKind::Table,
                    "V" => ObjectKind::View,
                    _ => ObjectKind::Function,
                };
                Some(DbObject {
                    schema: row.first()?.clone(),
                    name: row.get(1)?.clone()?,
                    kind,
                })
            })
            .collect())
    }

    /// The `CREATE PROCEDURE / FUNCTION …` text the server keeps.
    pub async fn routine_definition(&self, obj: &DbObject) -> Result<String> {
        let sql = format!(
            "select m.definition, case when o.object_id is null then 0 else 1 end \
             from (select {} as object_id) o left join sys.sql_modules m on m.object_id = o.object_id",
            self.object_id(obj)
        );
        let result = self.raw(&sql).await?;
        let row = result.rows.first();
        match row.and_then(|row| row.first().cloned().flatten()) {
            Some(definition) => Ok(definition.trim_end().to_string() + "\n"),
            None if row.and_then(|row| row.get(1).cloned().flatten()).as_deref() == Some("1") => {
                bail!(
                    "{} is encrypted: the server doesn't show its definition",
                    obj.name
                )
            }
            None => bail!("{} no longer exists", obj.name),
        }
    }

    pub async fn structure(&self, obj: &DbObject) -> Result<Structure> {
        let (columns, extras) = self.columns(obj).await?;
        let mut warnings = vec![];
        let mut part = |what: &str, e: Error| warnings.push(format!("{what}: {e:#}"));
        let indexes = self.indexes(obj).await.unwrap_or_else(|e| {
            part("Indexes", e);
            vec![]
        });
        let foreign_keys = self.foreign_keys(obj).await.unwrap_or_else(|e| {
            part("Foreign keys", e);
            vec![]
        });
        let referenced_by = self.referenced_by(obj).await.unwrap_or_else(|e| {
            part("Referencing tables", e);
            vec![]
        });
        let ddl = match obj.kind {
            ObjectKind::View => self.routine_definition(obj).await,
            _ => {
                self.table_ddl(obj, &columns, &extras, &indexes, &foreign_keys, None)
                    .await
            }
        }
        .unwrap_or_else(|e| {
            part("SQL", e);
            String::new()
        });
        let mut columns = columns;
        for (name, mut values) in check_in_lists(&ddl) {
            // The server keeps IN ('a', 'b', 'c') as c OR b OR a.
            values.reverse();
            if let Some(column) = columns
                .iter_mut()
                .find(|column| column.name == name && column.enum_values.is_empty())
            {
                column.enum_values = values;
            }
        }
        Ok(Structure {
            columns,
            indexes: QueryResult {
                columns: vec!["name".into(), "definition".into()],
                rows: indexes
                    .iter()
                    .map(|index| vec![Some(index.name.clone()), Some(index.definition())])
                    .collect(),
                ..Default::default()
            },
            foreign_keys,
            referenced_by,
            ddl,
            warnings,
        })
    }

    async fn columns(&self, obj: &DbObject) -> Result<(Vec<ColumnInfo>, Vec<ColumnExtra>)> {
        let sql = format!(
            "select c.name, {TYPE_SQL}, case when c.is_nullable = 1 then 'true' else 'false' end, \
             dc.definition, \
             case when exists (select 1 from sys.index_columns ic join sys.indexes i \
               on i.object_id = ic.object_id and i.index_id = ic.index_id \
               where i.is_primary_key = 1 and ic.object_id = c.object_id and ic.column_id = c.column_id) \
               then 'true' else 'false' end, \
             cast(ep.value as nvarchar(max)), \
             cast(idc.seed_value as nvarchar(40)), cast(idc.increment_value as nvarchar(40)), \
             cc.definition, dc.name, t.name \
             from sys.columns c join sys.types t on t.user_type_id = c.user_type_id \
             left join sys.default_constraints dc on dc.object_id = c.default_object_id \
             left join sys.identity_columns idc on idc.object_id = c.object_id and idc.column_id = c.column_id \
             left join sys.computed_columns cc on cc.object_id = c.object_id and cc.column_id = c.column_id \
             left join sys.extended_properties ep on ep.class = 1 and ep.major_id = c.object_id \
               and ep.minor_id = c.column_id and ep.name = 'MS_Description' \
             where c.object_id = {} order by c.column_id",
            self.object_id(obj)
        );
        let rows = self.raw(&sql).await?.rows;
        if rows.is_empty() {
            bail!("{} no longer exists", obj.title());
        }
        Ok(rows
            .into_iter()
            .map(|row| {
                let get = |index: usize| row.get(index).cloned().flatten();
                // Stored as "((0))": the expression is what's typed.
                let default = get(3).map(|d| unwrap_parens(&d).to_string());
                let info = ColumnInfo {
                    name: get(0).unwrap_or_default(),
                    data_type: get(1).unwrap_or_default(),
                    nullable: get(2).as_deref() == Some("true"),
                    default,
                    is_pk: get(4).as_deref() == Some("true"),
                    enum_values: vec![],
                    comment: get(5),
                };
                let extra = ColumnExtra {
                    identity: get(6).zip(get(7)),
                    computed: get(8),
                    default_name: get(9),
                    row_version: matches!(get(10).as_deref(), Some("timestamp" | "rowversion")),
                };
                (info, extra)
            })
            .unzip())
    }

    async fn indexes(&self, obj: &DbObject) -> Result<Vec<IndexInfo>> {
        let list = |included: u8| {
            format!(
                "stuff((select ', ' + quotename(col.name) + \
                 case when ic.is_descending_key = 1 then ' DESC' else '' end \
                 from sys.index_columns ic join sys.columns col \
                 on col.object_id = ic.object_id and col.column_id = ic.column_id \
                 where ic.object_id = i.object_id and ic.index_id = i.index_id \
                 and ic.is_included_column = {included} \
                 order by ic.key_ordinal, ic.index_column_id \
                 for xml path(''), type).value('.', 'nvarchar(max)'), 1, 2, '')"
            )
        };
        let sql = format!(
            "select i.name, i.type_desc, \
             case when i.is_primary_key = 1 then 'pk' when i.is_unique_constraint = 1 then 'uq' \
               when i.is_unique = 1 then 'unique' else '' end, \
             {}, {}, i.filter_definition \
             from sys.indexes i where i.object_id = {} and i.type > 0 \
             order by i.is_primary_key desc, i.name",
            list(0),
            list(1),
            self.object_id(obj)
        );
        Ok(self
            .raw(&sql)
            .await?
            .rows
            .into_iter()
            .map(|row| {
                let get = |index: usize| row.get(index).cloned().flatten();
                IndexInfo {
                    name: get(0).unwrap_or_default(),
                    kind: get(1).unwrap_or_default(),
                    role: get(2).unwrap_or_default(),
                    columns: get(3).unwrap_or_default(),
                    included: get(4),
                    filter: get(5),
                }
            })
            .collect())
    }

    async fn foreign_keys(&self, obj: &DbObject) -> Result<Vec<ForeignKey>> {
        let sql = format!(
            "select fk.name, pc.name, rs.name, rt.name, rc.name, \
             fk.update_referential_action_desc, fk.delete_referential_action_desc \
             from sys.foreign_keys fk \
             join sys.foreign_key_columns fkc on fkc.constraint_object_id = fk.object_id \
             join sys.columns pc on pc.object_id = fkc.parent_object_id and pc.column_id = fkc.parent_column_id \
             join sys.objects rt on rt.object_id = fkc.referenced_object_id \
             join sys.schemas rs on rs.schema_id = rt.schema_id \
             join sys.columns rc on rc.object_id = fkc.referenced_object_id \
               and rc.column_id = fkc.referenced_column_id \
             where fk.parent_object_id = {} order by fk.name, fkc.constraint_column_id",
            self.object_id(obj)
        );
        let mut out: Vec<ForeignKey> = vec![];
        for row in self.raw(&sql).await?.rows {
            let get = |index: usize| row.get(index).cloned().flatten().unwrap_or_default();
            if out.last().is_none_or(|key| key.name != get(0)) {
                out.push(ForeignKey {
                    name: get(0),
                    ref_schema: row.get(2).cloned().flatten(),
                    ref_table: get(3),
                    // NO_ACTION → NO ACTION, as the editors spell them.
                    on_update: get(5).replace('_', " "),
                    on_delete: get(6).replace('_', " "),
                    ..Default::default()
                });
            }
            let key = out.last_mut().expect("just pushed");
            key.columns.push(get(1));
            key.ref_columns.push(get(4));
        }
        Ok(out)
    }

    /// Other tables' foreign keys that reference `obj`, reversed (see
    /// `Structure::referenced_by`).
    async fn referenced_by(&self, obj: &DbObject) -> Result<Vec<ForeignKey>> {
        let sql = format!(
            "select fk.name, rc.name, ps.name, pt.name, pc.name \
             from sys.foreign_keys fk \
             join sys.foreign_key_columns fkc on fkc.constraint_object_id = fk.object_id \
             join sys.objects pt on pt.object_id = fkc.parent_object_id \
             join sys.schemas ps on ps.schema_id = pt.schema_id \
             join sys.columns pc on pc.object_id = fkc.parent_object_id and pc.column_id = fkc.parent_column_id \
             join sys.columns rc on rc.object_id = fkc.referenced_object_id \
               and rc.column_id = fkc.referenced_column_id \
             where fk.referenced_object_id = {} order by pt.name, fk.name, fkc.constraint_column_id",
            self.object_id(obj)
        );
        let mut out: Vec<ForeignKey> = vec![];
        for row in self.raw(&sql).await?.rows {
            let get = |index: usize| row.get(index).cloned().flatten().unwrap_or_default();
            let same_key = out
                .last()
                .is_some_and(|key| key.name == get(0) && key.ref_table == get(3));
            if !same_key {
                out.push(ForeignKey {
                    name: get(0),
                    ref_schema: row.get(2).cloned().flatten(),
                    ref_table: get(3),
                    ..Default::default()
                });
            }
            let key = out.last_mut().expect("just pushed");
            key.columns.push(get(1));
            key.ref_columns.push(get(4));
        }
        Ok(out)
    }

    /// SQL Server has no SHOW CREATE TABLE: rebuilt from the catalog. With
    /// `new_name`, for a copy of the table: constraints go unnamed, since
    /// their names must be unique in the schema.
    async fn table_ddl(
        &self,
        obj: &DbObject,
        columns: &[ColumnInfo],
        extras: &[ColumnExtra],
        indexes: &[IndexInfo],
        foreign_keys: &[ForeignKey],
        new_name: Option<&str>,
    ) -> Result<String> {
        let table = match new_name {
            Some(name) => self.full_name(&DbObject {
                name: name.to_string(),
                ..obj.clone()
            }),
            None => self.full_name(obj),
        };
        let named = |name: &str| match new_name {
            Some(_) => String::new(),
            None => format!("CONSTRAINT {} ", ident(name)),
        };
        let mut lines: Vec<String> = columns
            .iter()
            .zip(extras)
            .map(|(column, extra)| {
                if let Some(expression) = &extra.computed {
                    return format!("  {} AS {expression}", ident(&column.name));
                }
                let mut line = format!("  {} {}", ident(&column.name), column.data_type);
                if let Some((seed, increment)) = &extra.identity {
                    line.push_str(&format!(" IDENTITY({seed},{increment})"));
                }
                line.push_str(if column.nullable {
                    " NULL"
                } else {
                    " NOT NULL"
                });
                if let Some(default) = &column.default {
                    let name = extra.default_name.as_deref().map(named).unwrap_or_default();
                    line.push_str(&format!(" {name}DEFAULT ({default})"));
                }
                line
            })
            .collect();
        for index in indexes.iter().filter(|index| !index.role.is_empty()) {
            let what = match index.role.as_str() {
                "pk" => "PRIMARY KEY",
                "uq" => "UNIQUE",
                _ => continue,
            };
            lines.push(format!(
                "  {}{what} {} ({})",
                named(&index.name),
                index.kind,
                index.columns
            ));
        }
        for key in foreign_keys {
            let list = |names: &[String]| {
                names
                    .iter()
                    .map(|name| ident(name))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let mut line = format!(
                "  {}FOREIGN KEY ({}) REFERENCES {}.{} ({})",
                named(&key.name),
                list(&key.columns),
                ident(key.ref_schema.as_deref().unwrap_or("dbo")),
                ident(&key.ref_table),
                list(&key.ref_columns)
            );
            for (verb, action) in [("DELETE", &key.on_delete), ("UPDATE", &key.on_update)] {
                if !action.is_empty() && action != "NO ACTION" {
                    line.push_str(&format!(" ON {verb} {action}"));
                }
            }
            lines.push(line);
        }
        let checks = self
            .raw(&format!(
                "select name, definition from sys.check_constraints \
                 where parent_object_id = {} order by name",
                self.object_id(obj)
            ))
            .await?;
        for row in checks.rows {
            if let (Some(name), Some(definition)) = (&row[0], &row[1]) {
                lines.push(format!("  {}CHECK {definition}", named(name)));
            }
        }
        let mut out = format!("CREATE TABLE {table} (\n{}\n);\n", lines.join(",\n"));
        for index in indexes
            .iter()
            .filter(|index| index.role.is_empty() || index.role == "unique")
        {
            match index.create(&table) {
                Some(statement) => out.push_str(&format!("{statement};\n")),
                None => out.push_str(&format!(
                    "-- {} index {} isn't shown\n",
                    index.kind.to_lowercase(),
                    index.name
                )),
            }
        }
        Ok(out)
    }

    /// Sessions other than ours, first column = the id `kill` takes.
    pub async fn processes(&self) -> Result<QueryResult> {
        self.raw(
            "select s.session_id as id, s.login_name as [user], db_name(s.database_id) as [database], \
             s.host_name as client, s.program_name as app, coalesce(r.status, s.status) as state, \
             r.command, datediff(second, r.start_time, sysdatetime()) as seconds, \
             r.wait_type as waiting_on, nullif(r.blocking_session_id, 0) as blocked_by, \
             t.text as query \
             from sys.dm_exec_sessions s \
             left join sys.dm_exec_requests r on r.session_id = s.session_id \
             outer apply sys.dm_exec_sql_text(r.sql_handle) t \
             where s.is_user_process = 1 and s.session_id <> @@spid \
             order by r.start_time desc",
        )
        .await
    }

    /// SQL Server can only end the whole session (`KILL`), which also
    /// rolls back its open transaction.
    pub async fn kill(&self, id: &str) -> Result<()> {
        let id: u32 = id.parse().context("not a session id")?;
        self.raw(&format!("kill {id}")).await?;
        Ok(())
    }

    /// Up to `limit` rows of `obj` whose `key` starts with `text` or whose
    /// `labels` contain it (see `SqlConn::fk_matches`). Only reads.
    pub async fn fk_matches(
        &self,
        obj: &DbObject,
        key: &str,
        labels: &[String],
        text: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let as_text = |column: &str| format!("cast({} as nvarchar(max))", ident(column));
        let escaped = escape_like(text);
        // concat_ws without SQL Server 2017: `N' · ' + NULL` is NULL, which
        // concat skips, and stuff drops the first separator.
        let label = match labels {
            [] => "N''".to_string(),
            labels => format!(
                "stuff(concat({}), 1, 3, N'')",
                labels
                    .iter()
                    .map(|label| format!("N' · ' + {}", as_text(label)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        let mut sql = format!(
            "select top {limit} {} as tabula_key, {label} as tabula_label from {}",
            as_text(key),
            self.full_name(obj)
        );
        if !text.is_empty() {
            let mut conditions = vec![format!(
                "{} like {} escape '!'",
                as_text(key),
                lit(&format!("{escaped}%"))
            )];
            for label in labels {
                conditions.push(format!(
                    "{} like {} escape '!'",
                    as_text(label),
                    lit(&format!("%{escaped}%"))
                ));
            }
            sql += &format!(" where {}", conditions.join(" or "));
        }
        sql += &format!(" order by {}", ident(key));
        Ok(self
            .raw(&sql)
            .await?
            .rows
            .into_iter()
            .filter_map(|row| {
                let key = row.first().cloned().flatten()?;
                Some((key, row.get(1).cloned().flatten().unwrap_or_default()))
            })
            .collect())
    }

    fn where_clause(&self, q: &TableQuery) -> String {
        let parts: Vec<String> = q
            .filters
            .iter()
            .filter(|filter| !filter.column.is_empty())
            .filter_map(|filter| self.condition(filter))
            .collect();
        if parts.is_empty() {
            String::new()
        } else if q.match_any {
            format!(" where ({})", parts.join(") or ("))
        } else {
            format!(" where {}", parts.join(" and "))
        }
    }

    /// One filter as T-SQL; `None` when it has nothing to test yet. Text
    /// matching follows the column's collation (usually case-insensitive).
    pub fn condition(&self, f: &Filter) -> Option<String> {
        let column = ident(&f.column);
        let as_text = format!("cast({column} as nvarchar(max))");
        let value = lit(&f.value);
        let escaped = escape_like(&f.value);
        let pattern = |before: &str, after: &str| {
            format!(
                "{as_text} like {} escape '!'",
                lit(&format!("{before}{escaped}{after}"))
            )
        };
        let list = || {
            let items = FilterOp::list(&f.value);
            (!items.is_empty()).then(|| {
                items
                    .iter()
                    .map(|item| lit(item))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
        };
        Some(match f.op {
            FilterOp::Eq => format!("{column} = {value}"),
            FilterOp::Ne => format!("{column} <> {value}"),
            FilterOp::In => format!("{column} in ({})", list()?),
            FilterOp::NotIn => format!("{column} not in ({})", list()?),
            FilterOp::IsNull => format!("{column} is null"),
            FilterOp::IsNotNull => format!("{column} is not null"),
            FilterOp::Lt => format!("{column} < {value}"),
            FilterOp::Le => format!("{column} <= {value}"),
            FilterOp::Gt => format!("{column} > {value}"),
            FilterOp::Ge => format!("{column} >= {value}"),
            FilterOp::Between => format!("{column} between {value} and {}", lit(&f.value2)),
            FilterOp::Contains => pattern("%", "%"),
            FilterOp::NotContains => format!("not ({})", pattern("%", "%")),
            FilterOp::StartsWith => pattern("", "%"),
            FilterOp::EndsWith => pattern("%", ""),
            // No regex operator before SQL Server 2025; the editor doesn't
            // offer one.
            FilterOp::Like | FilterOp::Regex => format!("{as_text} like {value}"),
        })
    }

    pub fn where_preview(&self, q: &TableQuery) -> String {
        self.where_clause(q).trim_start().to_string()
    }

    pub async fn fetch_table(&self, q: &TableQuery) -> Result<(QueryResult, Option<u64>)> {
        let table = self.full_name(&q.object);
        let filters = self.where_clause(q);
        // OFFSET needs an ORDER BY; `(select null)` keeps the server's order.
        let order = match &q.order {
            Some((column, ascending)) => format!(
                " order by {} {}",
                ident(column),
                if *ascending { "asc" } else { "desc" }
            ),
            None => " order by (select null)".to_string(),
        };
        let sql = format!(
            "select * from {table}{filters}{order} offset {} rows fetch next {} rows only",
            q.offset,
            q.limit.max(1)
        );
        let page = self.raw(&sql).await?;
        let total = self
            .scalar(&format!("select count_big(*) from {table}{filters}"))
            .await
            .ok()
            .flatten()
            .and_then(|count| count.parse().ok());
        Ok((page, total))
    }

    /// Runs editor text on one connection, so `BEGIN TRAN … ROLLBACK`,
    /// `SET` and temp tables carry from one statement to the next. The batch
    /// runs in its own task: a cancel stops the statement with a TDS
    /// attention (no KILL permission needed), even after the caller has
    /// stopped waiting.
    pub async fn execute(&self, text: &str, cancel: &Cancel) -> Result<Vec<QueryResult>> {
        self.refuse_writes(text)?;
        let statements = split_mssql(text);
        let count = statements.len();
        let mut conn = self.pool.get().await?;
        let (stop_tx, mut stop_rx) = oneshot::channel::<oneshot::Sender<Result<(), String>>>();
        cancel.on_server_cancel(Box::new(move || {
            Box::pin(async move {
                let (ack_tx, ack_rx) = oneshot::channel();
                if stop_tx.send(ack_tx).is_err() {
                    // The batch already ended.
                    return Ok(());
                }
                ack_rx.await.unwrap_or(Ok(()))
            })
        }));
        let task = tokio_spawn(async move {
            let mut effects = SessionEffects::default();
            let mut done = Vec::new();
            let mut failed: Option<(usize, Error)> = None;
            let mut stopped = false;
            for (index, statement) in statements.into_iter().enumerate() {
                effects.observe(&statement);
                let start = Instant::now();
                select! {
                    result = run_one(conn.client(), &statement) => match result {
                        Ok(mut results) => {
                            for result in &mut results {
                                result.elapsed_ms = start.elapsed().as_millis();
                            }
                            done.extend(results);
                        }
                        Err(e) => {
                            failed = Some((index, e));
                            break;
                        }
                    },
                    Ok(ack) = &mut stop_rx => {
                        let outcome = match timeout(Duration::from_secs(5), conn.client().cancel_query()).await {
                            Ok(Ok(())) => Ok(()),
                            Ok(Err(e)) => Err(e.to_string()),
                            Err(_) => Err("the server didn't confirm the cancel".to_string()),
                        };
                        let _ = ack.send(outcome);
                        stopped = true;
                        failed = Some((index, anyhow!(CANCELLED)));
                        break;
                    }
                }
            }
            // A cancelled connection is closed, which rolls back its open
            // transaction. Otherwise one left open is rolled back here, as
            // the other drivers do, and reported.
            let mut rolled_back = false;
            if !stopped {
                let open = run_one(conn.client(), "select @@trancount")
                    .await
                    .ok()
                    .and_then(|results| results.into_iter().next())
                    .and_then(|result| result.rows.into_iter().next())
                    .and_then(|row| row.into_iter().next().flatten())
                    .map(|count| count != "0");
                rolled_back = open == Some(true);
                if rolled_back {
                    let _ = run_one(conn.client(), "rollback").await;
                }
                if open == Some(false) && !effects.dirty && failed.is_none() {
                    conn.release();
                }
            }
            BatchOutcome {
                done,
                failed,
                rolled_back,
            }
        });
        let BatchOutcome {
            done,
            failed,
            rolled_back,
        } = task
            .await
            .map_err(|e| anyhow!("the query stopped unexpectedly: {e}"))?;
        let note = if rolled_back {
            " The open transaction was rolled back."
        } else {
            ""
        };
        if let Some((index, e)) = failed {
            let ran = if index > 0 && !rolled_back {
                format!(" The {index} statement(s) before it were applied.")
            } else {
                String::new()
            };
            if cancel.is_cancelled() {
                let e = anyhow!(cancel.message());
                if count == 1 {
                    return Err(e);
                }
                return Err(e.context(BatchFailed {
                    index,
                    done,
                    message: format!("Stopped at statement {} of {count}.{ran}{note}", index + 1),
                }));
            }
            return Err(e.context(BatchFailed {
                index,
                done,
                message: format!("statement {} of {count} failed.{ran}{note}", index + 1),
            }));
        }
        if rolled_back {
            bail!(
                "BEGIN without COMMIT or ROLLBACK: nothing was saved.{note} \
                 End the batch with COMMIT; to keep the changes."
            );
        }
        Ok(done)
    }

    /// (sql, params, must affect exactly one row) per change. With `inline`,
    /// values are literals (for the preview) instead of `@Pn` parameters.
    fn statements(
        &self,
        obj: &DbObject,
        columns: &[ColumnInfo],
        changes: &[Change],
        inline: bool,
    ) -> Vec<(String, Vec<Option<String>>, bool)> {
        let table = self.full_name(obj);
        let binary = |column: &str| {
            columns
                .iter()
                .any(|info| info.name == column && is_binary_type(&info.data_type))
        };
        let mut out = Vec::new();
        for change in changes {
            let mut params: Vec<Option<String>> = Vec::new();
            // Parameters are text; the server converts them to the column's
            // type. Hex into a binary column means bytes.
            let mut value = |column: &str, v: &Option<String>| -> String {
                let hex = v
                    .as_deref()
                    .filter(|_| binary(column))
                    .and_then(hex_digits)
                    .map(str::to_string);
                match (hex, inline) {
                    (Some(digits), true) => format!("0x{digits}"),
                    (Some(digits), false) => {
                        params.push(Some(format!("0x{digits}")));
                        format!("convert(varbinary(max), @P{}, 1)", params.len())
                    }
                    (None, true) => v.as_deref().map(lit).unwrap_or_else(|| "NULL".into()),
                    (None, false) => {
                        params.push(v.clone());
                        format!("@P{}", params.len())
                    }
                }
            };
            let sql = match change {
                Change::Update { key, set } => {
                    let sets: Vec<String> = set
                        .iter()
                        .map(|(column, v)| format!("{} = {}", ident(column), value(column, v)))
                        .collect();
                    let filter = key_where(key, &mut value);
                    format!("update {table} set {} where {filter}", sets.join(", "))
                }
                Change::Insert { values } => {
                    let values: Vec<_> = values.iter().filter(|(_, v)| v.is_some()).collect();
                    if values.is_empty() {
                        format!("insert into {table} default values")
                    } else {
                        let names: Vec<_> =
                            values.iter().map(|(column, _)| ident(column)).collect();
                        let given: Vec<_> =
                            values.iter().map(|(column, v)| value(column, v)).collect();
                        format!(
                            "insert into {table} ({}) values ({})",
                            names.join(", "),
                            given.join(", ")
                        )
                    }
                }
                Change::Delete { key } => {
                    format!("delete from {table} where {}", key_where(key, &mut value))
                }
            };
            out.push((sql, params, true));
        }
        out
    }

    pub fn preview(&self, obj: &DbObject, columns: &[ColumnInfo], changes: &[Change]) -> String {
        self.statements(obj, columns, changes, true)
            .into_iter()
            .map(|(sql, _, _)| sql + ";")
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// All changes in one transaction; XACT_ABORT so any error undoes it.
    pub async fn commit(
        &self,
        obj: &DbObject,
        columns: &[ColumnInfo],
        changes: &[Change],
    ) -> Result<()> {
        self.refuse_writes("")?;
        let statements = self.statements(obj, columns, changes, false);
        let mut conn = self.pool.get().await?;
        let client = conn.client();
        run_one(client, "set xact_abort on; begin transaction").await?;
        for (sql, params, expect_one) in &statements {
            // The count comes back as a row: triggers' own DONE counts would
            // make the protocol's total misleading.
            let mut query = Query::new(format!("{sql}; select rowcount_big()"));
            for param in params {
                query.bind(param.as_deref());
            }
            let affected = async {
                let results = query
                    .query(client)
                    .await
                    .map_err(tidy)?
                    .into_results()
                    .await
                    .map_err(tidy)?;
                Ok::<_, Error>(
                    results
                        .last()
                        .and_then(|rows| rows.first())
                        .and_then(|row| row.get::<i64, _>(0))
                        .unwrap_or(0),
                )
            }
            .await
            .with_context(|| sql.clone());
            let affected = match affected {
                Ok(affected) => affected,
                Err(e) => {
                    let _ = run_one(client, "if @@trancount > 0 rollback").await;
                    return Err(e);
                }
            };
            if *expect_one && affected != 1 {
                let _ = run_one(client, "if @@trancount > 0 rollback").await;
                bail!("{sql}\naffected {affected} rows instead of 1; nothing was committed");
            }
        }
        run_one(client, "commit; set xact_abort off").await?;
        conn.release();
        Ok(())
    }

    pub async fn drop_object(&self, obj: &DbObject) -> Result<()> {
        let kind = match obj.kind {
            ObjectKind::View => "view",
            _ => "table",
        };
        self.raw(&format!("drop {kind} {}", self.full_name(obj)))
            .await?;
        Ok(())
    }

    pub async fn create_database(&self, name: &str) -> Result<()> {
        self.raw(&format!("create database {}", ident(name)))
            .await?;
        Ok(())
    }

    pub async fn drop_database(&self, name: &str) -> Result<()> {
        if name == self.database {
            bail!("switch to another database before dropping this one");
        }
        self.raw(&format!("drop database {}", ident(name))).await?;
        Ok(())
    }

    /// `sp_rename` takes the new name bare: no schema, no brackets.
    pub async fn rename_object(&self, obj: &DbObject, new_name: &str) -> Result<()> {
        self.raw(&format!(
            "exec sp_rename {}, {}",
            lit(&self.full_name(obj)),
            lit(new_name)
        ))
        .await?;
        Ok(())
    }

    /// Recreates the table from its DDL under `new_name` (keys, defaults,
    /// checks and indexes included) and, with `data`, copies the rows, in
    /// one transaction.
    pub async fn duplicate_table(&self, obj: &DbObject, new_name: &str, data: bool) -> Result<()> {
        let (columns, extras) = self.columns(obj).await?;
        let indexes = self.indexes(obj).await?;
        let foreign_keys = self.foreign_keys(obj).await?;
        let ddl = self
            .table_ddl(
                obj,
                &columns,
                &extras,
                &indexes,
                &foreign_keys,
                Some(new_name),
            )
            .await?;
        let target = self.full_name(&DbObject {
            name: new_name.to_string(),
            ..obj.clone()
        });
        let mut script = format!("set xact_abort on;\nbegin transaction;\n{ddl}");
        if data {
            // Computed and rowversion columns fill themselves in.
            let copied: Vec<String> = columns
                .iter()
                .zip(&extras)
                .filter(|(_, extra)| extra.computed.is_none() && !extra.row_version)
                .map(|(column, _)| ident(&column.name))
                .collect();
            let list = copied.join(", ");
            let identity = extras.iter().any(|extra| extra.identity.is_some());
            if identity {
                script += &format!("set identity_insert {target} on;\n");
            }
            script += &format!(
                "insert into {target} ({list}) select {list} from {};\n",
                self.full_name(obj)
            );
            if identity {
                script += &format!("set identity_insert {target} off;\n");
            }
        }
        script += "commit;";
        self.execute(&script, &Cancel::default()).await?;
        Ok(())
    }

    /// Refreshes the table's statistics (index rebuilds can take long and
    /// lock the table, so they're left to the user).
    pub async fn maintain(&self, obj: &DbObject) -> Result<String> {
        self.raw(&format!("update statistics {}", self.full_name(obj)))
            .await?;
        Ok("Updated statistics".to_string())
    }

    pub async fn truncate(&self, obj: &DbObject) -> Result<()> {
        self.raw(&format!("truncate table {}", self.full_name(obj)))
            .await?;
        Ok(())
    }

    pub async fn close(&self) {
        self.pool.close();
    }
}

/// `key = value and …`, `is null` for NULL parts.
fn key_where(
    key: &[(String, Option<String>)],
    value: &mut impl FnMut(&str, &Option<String>) -> String,
) -> String {
    key.iter()
        .map(|(column, v)| match v {
            None => format!("{} is null", ident(column)),
            Some(_) => format!("{} = {}", ident(column), value(column, v)),
        })
        .collect::<Vec<_>>()
        .join(" and ")
}

/// LIKE wildcards typed by the user are literal (with `escape '!'`); `[`
/// is a wildcard in T-SQL too.
fn escape_like(text: &str) -> String {
    text.replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_")
        .replace('[', "![")
}

/// One index as the catalog describes it.
struct IndexInfo {
    name: String,
    /// CLUSTERED, NONCLUSTERED, CLUSTERED COLUMNSTORE, XML…
    kind: String,
    /// "pk" / "uq" (constraints), "unique", or "" for a plain index.
    role: String,
    /// `[a], [b] DESC`.
    columns: String,
    included: Option<String>,
    filter: Option<String>,
}

impl IndexInfo {
    /// For the Structure view's index list.
    fn definition(&self) -> String {
        let role = match self.role.as_str() {
            "pk" => "PRIMARY KEY ",
            "uq" | "unique" => "UNIQUE ",
            _ => "",
        };
        let mut text = format!("{role}{} ({})", self.kind, self.columns);
        if let Some(included) = &self.included {
            text += &format!(" INCLUDE ({included})");
        }
        if let Some(filter) = &self.filter {
            text += &format!(" WHERE {filter}");
        }
        text
    }

    /// `CREATE INDEX …` for a row-store index; `None` for the kinds that
    /// need more than a column list (columnstore, XML, spatial).
    fn create(&self, table: &str) -> Option<String> {
        if !matches!(self.kind.as_str(), "CLUSTERED" | "NONCLUSTERED") {
            return None;
        }
        let unique = if self.role == "unique" { "UNIQUE " } else { "" };
        let mut statement = format!(
            "CREATE {unique}{} INDEX {} ON {table} ({})",
            self.kind,
            ident(&self.name),
            self.columns
        );
        if let Some(included) = &self.included {
            statement += &format!(" INCLUDE ({included})");
        }
        if let Some(filter) = &self.filter {
            statement += &format!(" WHERE {filter}");
        }
        Some(statement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_and_fractions() {
        assert_eq!(ident("a]b"), "[a]]b]");
        assert_eq!(lit("it's ünïcode"), "N'it''s ünïcode'");
        assert_eq!(fraction(0), "");
        assert_eq!(fraction(123_000_000), ".123");
        assert_eq!(fraction(123_456_789), ".1234567");
        assert_eq!(escape_like("50%_[x]!"), "50!%!_![x]!!");
    }

    #[test]
    fn values_read_as_text() {
        assert_eq!(cell(&ColumnData::Bit(Some(true))).as_deref(), Some("1"));
        assert_eq!(cell(&ColumnData::I32(None)), None);
        assert_eq!(
            cell(&ColumnData::Binary(Some(vec![0xde, 0xad].into()))).as_deref(),
            Some("0xdead")
        );
        let when = NaiveDate::from_ymd_opt(2026, 11, 8)
            .and_then(|date| date.and_hms_milli_opt(9, 30, 5, 250))
            .expect("valid date");
        assert_eq!(date_time_text(when), "2026-11-08 09:30:05.25");
        // 299, 1 and 2 ticks of 1/300 s: .997, .003, .007.
        for (ticks, shown) in [
            (299, "1900-01-01 00:00:00.997"),
            (1, "1900-01-01 00:00:00.003"),
            (2, "1900-01-01 00:00:00.007"),
            (300, "1900-01-01 00:00:01"),
        ] {
            let value = tiberius::time::DateTime::new(0, ticks);
            assert_eq!(
                legacy_date_time(value).map(date_time_text).as_deref(),
                Some(shown)
            );
        }
    }
}
