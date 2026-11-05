//! Database layer: one `Connection` enum that hides the five drivers behind a
//! common, string-based API the UI can render directly.

pub mod cancel;
pub mod ddl;
pub mod format;
#[cfg(test)]
mod it;
mod mongo;
mod redis_kv;
pub mod safety;
pub mod split;
mod sql;
pub mod ssh;
pub mod target;

use std::time::Instant;

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use cancel::Cancel;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Driver {
    Postgres,
    MySql,
    Sqlite,
    Redis,
    Mongo,
}

impl Driver {
    pub const ALL: [Driver; 5] = [
        Driver::Postgres,
        Driver::MySql,
        Driver::Sqlite,
        Driver::Redis,
        Driver::Mongo,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Driver::Postgres => "PostgreSQL",
            Driver::MySql => "MySQL / MariaDB",
            Driver::Sqlite => "SQLite",
            Driver::Redis => "Redis",
            Driver::Mongo => "MongoDB",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Driver::Postgres => "Pg",
            Driver::MySql => "My",
            Driver::Sqlite => "Lt",
            Driver::Redis => "Rd",
            Driver::Mongo => "Mg",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Driver::Postgres => 5432,
            Driver::MySql => 3306,
            Driver::Sqlite => 0,
            Driver::Redis => 6379,
            Driver::Mongo => 27017,
        }
    }

    /// Language id for the editor's syntax highlighter.
    pub fn editor_language(self) -> &'static str {
        match self {
            Driver::Mongo => "json",
            Driver::Redis => "text",
            _ => "sql",
        }
    }
}

/// Environment tag, shown as a colored badge like TablePlus. `Production`
/// turns on safe mode: every commit and query asks for confirmation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvTag {
    #[default]
    Local,
    Development,
    Testing,
    Staging,
    Production,
}

impl EnvTag {
    pub const ALL: [EnvTag; 5] = [
        EnvTag::Local,
        EnvTag::Development,
        EnvTag::Testing,
        EnvTag::Staging,
        EnvTag::Production,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EnvTag::Local => "local",
            EnvTag::Development => "development",
            EnvTag::Testing => "testing",
            EnvTag::Staging => "staging",
            EnvTag::Production => "production",
        }
    }

    /// 0xRRGGBB
    pub fn color(self) -> u32 {
        match self {
            EnvTag::Local => 0x8e8e93,
            EnvTag::Development => 0x34c759,
            EnvTag::Testing => 0x5ac8fa,
            EnvTag::Staging => 0xff9f0a,
            EnvTag::Production => 0xff453a,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub id: String,
    pub name: String,
    pub driver: Driver,
    #[serde(default)]
    pub tag: EnvTag,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub database: String,
    /// SQLite file path.
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub ssl: bool,
    /// With `ssl`: also check the server certificate and host name, so an
    /// impostor server can't read the password. Off by default because it
    /// rejects self-signed / private-CA servers.
    #[serde(default)]
    pub ssl_verify: bool,
    /// Lives in the OS keychain, never in the JSON file.
    #[serde(skip)]
    pub password: String,
    /// Connect through an SSH tunnel.
    #[serde(default)]
    pub ssh: Option<ssh::SshConfig>,
    #[serde(default)]
    pub safe_mode: SafeMode,
}

/// How carefully Tabula treats writes on a connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SafeMode {
    /// Confirm writes on Production-tagged connections, nothing elsewhere.
    #[default]
    Auto,
    Off,
    /// Ask before running anything that writes, and before commits.
    ConfirmWrites,
    /// Ask before running any query.
    ConfirmAll,
    /// Refuse writes (also enforced by the server session where possible).
    ReadOnly,
}

impl SafeMode {
    pub const ALL: [SafeMode; 5] = [
        SafeMode::Auto,
        SafeMode::Off,
        SafeMode::ConfirmWrites,
        SafeMode::ConfirmAll,
        SafeMode::ReadOnly,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SafeMode::Auto => "Automatic (confirm writes on Production)",
            SafeMode::Off => "Off",
            SafeMode::ConfirmWrites => "Confirm writes",
            SafeMode::ConfirmAll => "Confirm every query",
            SafeMode::ReadOnly => "Read-only",
        }
    }
}

impl ConnectionConfig {
    pub fn new(driver: Driver) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            driver,
            tag: EnvTag::Local,
            host: "127.0.0.1".into(),
            port: driver.default_port(),
            user: String::new(),
            database: String::new(),
            file: String::new(),
            ssl: false,
            ssl_verify: false,
            password: String::new(),
            ssh: None,
            safe_mode: SafeMode::Auto,
        }
    }

    /// The safe mode in force (`Auto` resolved by the tag).
    pub fn safety(&self) -> SafeMode {
        match (self.safe_mode, self.tag) {
            (SafeMode::Auto, EnvTag::Production) => SafeMode::ConfirmWrites,
            (SafeMode::Auto, _) => SafeMode::Off,
            (m, _) => m,
        }
    }

    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        match self.driver {
            Driver::Sqlite => self.file.rsplit('/').next().unwrap_or("sqlite").to_string(),
            _ => format!("{}:{}", self.host, self.port),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Table,
    View,
    Collection,
    Keys,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbObject {
    pub schema: Option<String>,
    pub name: String,
    pub kind: ObjectKind,
}

impl DbObject {
    pub fn title(&self) -> String {
        match &self.schema {
            Some(s) if s != "public" && s != "main" => format!("{s}.{}", self.name),
            _ => self.name.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub is_pk: bool,
}

/// A foreign key: `columns` of this table reference `ref_columns` of
/// `ref_table`, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_update: String,
    pub on_delete: String,
}

/// What the Structure view shows.
#[derive(Clone, Debug, Default)]
pub struct Structure {
    pub columns: Vec<ColumnInfo>,
    pub indexes: QueryResult,
    pub foreign_keys: Vec<ForeignKey>,
    /// `CREATE TABLE …` (plus indexes where the server keeps them apart).
    pub ddl: String,
}

#[derive(Clone, Debug, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub affected: Option<u64>,
    pub elapsed_ms: u128,
    /// The statement this result belongs to, for multi-statement runs.
    pub statement: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Contains,
    NotContains,
    IsNull,
    IsNotNull,
}

impl FilterOp {
    pub const ALL: [FilterOp; 10] = [
        FilterOp::Eq,
        FilterOp::Ne,
        FilterOp::Lt,
        FilterOp::Gt,
        FilterOp::Le,
        FilterOp::Ge,
        FilterOp::Contains,
        FilterOp::NotContains,
        FilterOp::IsNull,
        FilterOp::IsNotNull,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FilterOp::Eq => "=",
            FilterOp::Ne => "<>",
            FilterOp::Lt => "<",
            FilterOp::Gt => ">",
            FilterOp::Le => "<=",
            FilterOp::Ge => ">=",
            FilterOp::Contains => "contains",
            FilterOp::NotContains => "not contains",
            FilterOp::IsNull => "is NULL",
            FilterOp::IsNotNull => "is not NULL",
        }
    }

    pub fn needs_value(self) -> bool {
        !matches!(self, FilterOp::IsNull | FilterOp::IsNotNull)
    }
}

#[derive(Clone, Debug)]
pub struct Filter {
    pub column: String,
    pub op: FilterOp,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct TableQuery {
    pub object: DbObject,
    pub filters: Vec<Filter>,
    pub order: Option<(String, bool)>, // (column, ascending)
    pub limit: usize,
    pub offset: usize,
}

/// A staged edit. `key` identifies the row: primary-key columns for SQL,
/// `_id` for Mongo, the key name for Redis.
#[derive(Clone, Debug)]
pub enum Change {
    Update {
        key: Vec<(String, Option<String>)>,
        set: Vec<(String, Option<String>)>,
    },
    Insert {
        values: Vec<(String, Option<String>)>,
    },
    Delete {
        key: Vec<(String, Option<String>)>,
    },
}

enum Conn {
    Sql(sql::SqlConn),
    Redis(redis_kv::RedisConn),
    Mongo(mongo::MongoConn),
}

/// An open connection. With SSH, the drivers talk to a local port that the
/// tunnel forwards; the tunnel closes when this is dropped.
pub struct Connection {
    inner: Conn,
    _tunnel: Option<ssh::Tunnel>,
}

impl Connection {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Self> {
        // Over SSH, the driver connects to the tunnel's local end instead.
        let (tunnel, cfg) = match &cfg.ssh {
            Some(ssh) if cfg.driver != Driver::Sqlite => {
                let t = ssh::Tunnel::open(ssh, &cfg.host, cfg.port).await?;
                let mut local = cfg.clone();
                local.host = "127.0.0.1".into();
                local.port = t.port;
                (Some(t), local)
            }
            _ => (None, cfg.clone()),
        };
        let cfg = &cfg;
        let inner = match cfg.driver {
            Driver::Postgres | Driver::MySql | Driver::Sqlite => {
                Conn::Sql(sql::SqlConn::connect(cfg).await?)
            }
            Driver::Redis => Conn::Redis(redis_kv::RedisConn::connect(cfg).await?),
            Driver::Mongo => Conn::Mongo(mongo::MongoConn::connect(cfg).await?),
        };
        Ok(Connection {
            inner,
            _tunnel: tunnel,
        })
    }

    pub async fn list_objects(&self) -> Result<Vec<DbObject>> {
        match &self.inner {
            Conn::Sql(c) => c.list_objects().await,
            Conn::Redis(c) => c.list_objects().await,
            Conn::Mongo(c) => c.list_objects().await,
        }
    }

    /// Every (table, column) in the current database, for autocomplete.
    /// Only SQL backends have a fixed schema; the others return nothing.
    pub async fn all_columns(&self) -> Result<Vec<(String, String, String)>> {
        match &self.inner {
            Conn::Sql(c) => c.all_columns().await,
            Conn::Redis(_) | Conn::Mongo(_) => Ok(vec![]),
        }
    }

    /// The Postgres schema chosen with `set_schema`, if any.
    pub fn current_schema(&self) -> Option<String> {
        match &self.inner {
            Conn::Sql(c) => c.current_schema(),
            _ => None,
        }
    }

    /// Postgres: make `schema` the default for unqualified names (None =
    /// server default). No-op for other databases.
    pub async fn set_schema(&mut self, schema: Option<String>) -> Result<()> {
        match &mut self.inner {
            Conn::Sql(c) => c.set_schema(schema).await,
            _ => Ok(()),
        }
    }

    pub async fn databases(&self) -> Result<Vec<String>> {
        match &self.inner {
            Conn::Sql(c) => c.databases().await,
            Conn::Redis(c) => c.databases().await,
            Conn::Mongo(c) => c.databases().await,
        }
    }

    /// Switch the active database / schema (like ⌘K in TablePlus).
    pub async fn use_database(&mut self, name: &str) -> Result<()> {
        match &mut self.inner {
            Conn::Sql(c) => c.use_database(name).await,
            Conn::Redis(c) => c.use_database(name).await,
            Conn::Mongo(c) => c.use_database(name),
        }
    }

    pub fn current_database(&self) -> String {
        match &self.inner {
            Conn::Sql(c) => c.current_database(),
            Conn::Redis(c) => c.current_database(),
            Conn::Mongo(c) => c.current_database(),
        }
    }

    pub async fn structure(&self, obj: &DbObject) -> Result<Structure> {
        match &self.inner {
            Conn::Sql(c) => c.structure(obj).await,
            Conn::Redis(c) => c.structure(obj),
            Conn::Mongo(c) => c.structure(obj).await,
        }
    }

    /// Returns the page of rows plus the total row count (for the pager).
    pub async fn fetch_table(&self, q: &TableQuery) -> Result<(QueryResult, Option<u64>)> {
        let start = Instant::now();
        let (mut res, total) = match &self.inner {
            Conn::Sql(c) => c.fetch_table(q).await?,
            Conn::Redis(c) => c.fetch_table(q).await?,
            Conn::Mongo(c) => c.fetch_table(q).await?,
        };
        res.elapsed_ms = start.elapsed().as_millis();
        Ok((res, total))
    }

    /// Runs editor text. SQL is split into statements; one result per statement.
    #[cfg(test)]
    pub async fn execute(&self, text: &str) -> Result<Vec<QueryResult>> {
        self.execute_cancellable(text, &Cancel::default()).await
    }

    /// `execute` that `cancel` can stop (see `cancel::Cancel`).
    pub async fn execute_cancellable(
        &self,
        text: &str,
        cancel: &Cancel,
    ) -> Result<Vec<QueryResult>> {
        let run = async {
            match &self.inner {
                Conn::Sql(c) => c.execute(text, cancel).await,
                Conn::Redis(c) => c.execute(text).await,
                Conn::Mongo(c) => c.execute(text).await,
            }
        };
        // SQL batches watch `cancel` themselves, to clean up the connection.
        tokio::select! {
            r = run => r,
            _ = cancel.cancelled() => Err(anyhow::anyhow!(cancel::CANCELLED)),
        }
    }

    /// Human-readable preview of what `commit` will run.
    pub fn preview(&self, obj: &DbObject, columns: &[ColumnInfo], changes: &[Change]) -> String {
        match &self.inner {
            Conn::Sql(c) => c.preview(obj, columns, changes),
            Conn::Redis(_) => redis_kv::preview(changes),
            Conn::Mongo(_) => mongo::preview(obj, changes),
        }
    }

    /// Applies all staged changes atomically where the backend allows it.
    pub async fn commit(
        &self,
        obj: &DbObject,
        columns: &[ColumnInfo],
        changes: &[Change],
    ) -> Result<()> {
        match &self.inner {
            Conn::Sql(c) => c.commit(obj, columns, changes).await,
            Conn::Redis(c) => c.commit(changes).await,
            Conn::Mongo(c) => c.commit(obj, changes).await,
        }
    }

    /// Whether `commit` is all-or-nothing. MongoDB (standalone) isn't: a
    /// failed commit may have saved some changes.
    pub fn commits_atomically(&self) -> bool {
        !matches!(self.inner, Conn::Mongo(_))
    }

    /// Server sessions (first column = id), for the process list.
    pub async fn processes(&self) -> Result<QueryResult> {
        match &self.inner {
            Conn::Sql(c) => c.processes().await,
            Conn::Redis(c) => c.processes().await,
            Conn::Mongo(_) => anyhow::bail!("not available for MongoDB yet"),
        }
    }

    /// Cancels a session's statement, or with `whole` ends the session.
    pub async fn kill(&self, id: &str, whole: bool) -> Result<()> {
        match &self.inner {
            Conn::Sql(c) => c.kill(id, whole).await,
            Conn::Redis(c) => c.kill(id).await,
            Conn::Mongo(_) => anyhow::bail!("not available for MongoDB yet"),
        }
    }

    pub async fn drop_object(&self, obj: &DbObject) -> Result<()> {
        match &self.inner {
            Conn::Sql(c) => c.drop_object(obj).await,
            Conn::Redis(_) => anyhow::bail!("not supported for Redis"),
            Conn::Mongo(c) => c.drop_object(obj).await,
        }
    }

    pub async fn truncate(&self, obj: &DbObject) -> Result<()> {
        match &self.inner {
            Conn::Sql(c) => c.truncate(obj).await,
            Conn::Redis(_) => anyhow::bail!("not supported for Redis"),
            Conn::Mongo(c) => c.truncate(obj).await,
        }
    }

    pub async fn close(&self) {
        match &self.inner {
            Conn::Sql(c) => c.close().await,
            Conn::Redis(_) | Conn::Mongo(_) => {}
        }
    }
}

/// Every value goes to the grid as text; turn a result cell into a quoted
/// literal for SQL previews / exports.
pub fn sql_literal(v: &Option<String>) -> String {
    match v {
        None => "NULL".into(),
        Some(s) => format!("'{}'", s.replace('\'', "''")),
    }
}
