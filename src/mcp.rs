//! The MCP server (Settings › MCP): lets AI tools (Claude Code, Cursor, …)
//! work with the saved connections the way the UI does — list databases and
//! tables, read structure and rows, run queries, change rows, see and stop
//! server processes.
//!
//! Streamable HTTP on 127.0.0.1 only, behind a bearer token (kept in the
//! keychain; Settings copies it for the tool's config). Loopback Host checks
//! guard against DNS rebinding. Every write passes the connection's safe
//! mode: read-only blocks it, "confirm" modes ask in the app window (the
//! same confirmation the UI shows) and the tool waits for the answer.
//! Passwords are never sent back.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value, json, to_string_pretty};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::spawn_blocking;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::cancel::Cancel;
use crate::db::safety::{Gate, gate_query, gate_write};
use crate::db::{
    Change, Connection, ConnectionConfig, DbObject, Filter, FilterOp, ObjectKind, QueryResult,
    TableQuery,
};
use crate::store;

/// Rows a single tool call returns at most; more come with `offset`.
const ROW_CAP: usize = 1000;

/// How long a write waits for its confirmation in the app.
const CONFIRM_WAIT: Duration = Duration::from_secs(300);

/// A write that needs the user's OK in the app window (safe mode).
pub struct ConfirmRequest {
    pub cfg: ConnectionConfig,
    /// "Run query", "Save changes", …
    pub what: String,
    /// What will run, shown in the confirmation.
    pub statements: String,
    /// `true` when confirmed; dropped (or `false`) when cancelled.
    pub reply: oneshot::Sender<bool>,
}

/// Where the saved connections come from (`store::load` in the app).
pub type Connections = Arc<dyn Fn() -> Vec<ConnectionConfig> + Send + Sync>;

/// A running server; stops when dropped.
pub struct Running {
    pub port: u16,
    token: Arc<StdRwLock<String>>,
    stop: CancellationToken,
}

impl Running {
    /// Replaces the token (Regenerate): tools using the old one are cut
    /// off at once, and the port stays the same.
    pub fn set_token(&self, token: String) {
        if let Ok(mut current) = self.token.write() {
            *current = token;
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// A fresh random token (two v4 UUIDs: 244 random bits).
pub fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// The URL tools connect to.
pub fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

/// Starts the server on `preferred_port` when it's free (so tools keep
/// working across restarts), otherwise on any free port.
pub async fn start(
    preferred_port: u16,
    token: String,
    confirm: mpsc::UnboundedSender<ConfirmRequest>,
    connections: Connections,
) -> Result<Running> {
    let loopback = |port| SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = match preferred_port {
        0 => TcpListener::bind(loopback(0)).await,
        port => match TcpListener::bind(loopback(port)).await {
            Ok(listener) => Ok(listener),
            Err(_) => TcpListener::bind(loopback(0)).await,
        },
    }
    .context("couldn't open a port for the MCP server")?;
    let port = listener.local_addr()?.port();
    let stop = CancellationToken::new();
    let pool = Arc::new(Mutex::new(HashMap::new()));
    let service: StreamableHttpService<Tabula, LocalSessionManager> = StreamableHttpService::new(
        move || {
            Ok(Tabula::new(
                pool.clone(),
                confirm.clone(),
                connections.clone(),
            ))
        },
        Default::default(),
        StreamableHttpServerConfig::default().with_cancellation_token(stop.child_token()),
    );
    let token = Arc::new(StdRwLock::new(token));
    let router = Router::new()
        .nest_service("/mcp", service)
        .layer(from_fn_with_state(token.clone(), check_token));
    let until = stop.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { until.cancelled_owned().await })
            .await;
    });
    Ok(Running { port, token, stop })
}

/// Every request carries `Authorization: Bearer <token>`.
async fn check_token(
    State(token): State<Arc<StdRwLock<String>>>,
    request: Request,
    next: Next,
) -> Response {
    let token = token.read().map(|token| token.clone()).unwrap_or_default();
    let given = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    match given {
        Some(given) if !token.is_empty() && same(given.as_bytes(), token.as_bytes()) => {
            next.run(request).await
        }
        _ => (
            StatusCode::UNAUTHORIZED,
            "Missing or wrong token: copy it from Tabula › Settings › MCP",
        )
            .into_response(),
    }
}

/// Compares without stopping at the first difference (timing).
fn same(given: &[u8], expected: &[u8]) -> bool {
    given.len() == expected.len()
        && given
            .iter()
            .zip(expected)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ConnectionArgs {
    /// Connection name or id, from list_connections.
    connection: String,
    /// Database to use instead of the connection's default (Redis: 0–15).
    #[serde(default)]
    database: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ObjectArgs {
    /// Connection name or id, from list_connections.
    connection: String,
    /// Database to use instead of the connection's default.
    #[serde(default)]
    database: Option<String>,
    /// Table, view, collection or function name ("keys" for Redis).
    object: String,
    /// Schema (Postgres), when the name is in more than one.
    #[serde(default)]
    schema: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FilterArgs {
    column: String,
    /// eq, ne, in, not_in, is_null, is_not_null, lt, le, gt, ge, between,
    /// contains, not_contains, starts_with, ends_with, like, regex.
    op: String,
    /// The value; for `in` / `not_in` a comma-separated list.
    #[serde(default)]
    value: String,
    /// The upper bound for `between`.
    #[serde(default)]
    value2: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadArgs {
    /// Connection name or id, from list_connections.
    connection: String,
    #[serde(default)]
    database: Option<String>,
    /// Table, view or collection ("keys" for Redis).
    object: String,
    #[serde(default)]
    schema: Option<String>,
    /// Conditions; all must match unless match_any is true.
    #[serde(default)]
    filters: Vec<FilterArgs>,
    #[serde(default)]
    match_any: bool,
    #[serde(default)]
    order_by: Option<String>,
    #[serde(default)]
    descending: bool,
    /// At most 1000 (the default 100).
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: usize,
    /// MongoDB: a filter document in JSON5, like { status: "paid" }.
    #[serde(default)]
    find: Option<String>,
    /// MongoDB: only these fields (and _id).
    #[serde(default)]
    fields: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct QueryArgs {
    /// Connection name or id, from list_connections.
    connection: String,
    #[serde(default)]
    database: Option<String>,
    /// SQL (several statements allowed), Redis commands (one per line) or
    /// MongoDB shell calls like db.orders.find({}).
    query: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ChangeArgs {
    /// insert, update or delete.
    action: String,
    /// update / delete: the row's key (primary-key columns; _id for
    /// MongoDB; { "key": name } for Redis).
    #[serde(default)]
    key: Map<String, Value>,
    /// insert / update: column → new value (null for NULL).
    #[serde(default)]
    values: Map<String, Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ChangeRowsArgs {
    /// Connection name or id, from list_connections.
    connection: String,
    #[serde(default)]
    database: Option<String>,
    /// Table or collection ("keys" for Redis).
    object: String,
    #[serde(default)]
    schema: Option<String>,
    /// Applied together in one transaction where the database allows it.
    changes: Vec<ChangeArgs>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct KillArgs {
    /// Connection name or id, from list_connections.
    connection: String,
    /// The process / session id, from list_processes.
    id: String,
    /// End the whole session instead of only its running statement.
    #[serde(default)]
    whole_session: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct KeyArgs {
    /// Connection name or id (a Redis connection).
    connection: String,
    #[serde(default)]
    database: Option<String>,
    key: String,
}

/// Open connections, one per (connection id, database).
type Pool = Arc<Mutex<HashMap<String, Arc<Connection>>>>;

#[derive(Clone)]
struct Tabula {
    tool_router: ToolRouter<Self>,
    pool: Pool,
    confirm: mpsc::UnboundedSender<ConfirmRequest>,
    connections: Connections,
}

fn filter_op(name: &str) -> Result<FilterOp> {
    Ok(match name.trim().to_ascii_lowercase().as_str() {
        "eq" | "=" => FilterOp::Eq,
        "ne" | "!=" => FilterOp::Ne,
        "in" => FilterOp::In,
        "not_in" => FilterOp::NotIn,
        "is_null" => FilterOp::IsNull,
        "is_not_null" => FilterOp::IsNotNull,
        "lt" | "<" => FilterOp::Lt,
        "le" | "<=" => FilterOp::Le,
        "gt" | ">" => FilterOp::Gt,
        "ge" | ">=" => FilterOp::Ge,
        "between" => FilterOp::Between,
        "contains" => FilterOp::Contains,
        "not_contains" => FilterOp::NotContains,
        "starts_with" => FilterOp::StartsWith,
        "ends_with" => FilterOp::EndsWith,
        "like" => FilterOp::Like,
        "regex" => FilterOp::Regex,
        other => bail!("unknown filter op {other:?}"),
    })
}

/// A JSON value as the text the drivers take (null → NULL).
fn cell(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

fn cells(map: &Map<String, Value>) -> Vec<(String, Option<String>)> {
    map.iter()
        .map(|(column, value)| (column.clone(), cell(value)))
        .collect()
}

/// Rows as objects ({column: value}), at most `ROW_CAP`.
fn rows_json(result: &QueryResult) -> Value {
    let rows: Vec<Value> = result
        .rows
        .iter()
        .take(ROW_CAP)
        .map(|row| {
            let object: Map<String, Value> = result
                .columns
                .iter()
                .zip(row)
                .map(|(column, value)| {
                    (
                        column.clone(),
                        value.clone().map_or(Value::Null, Value::String),
                    )
                })
                .collect();
            Value::Object(object)
        })
        .collect();
    json!({
        "columns": result.columns,
        "rows": rows,
        "truncated": result.rows.len() > ROW_CAP,
        "affected": result.affected,
    })
}

fn kind_name(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Table => "table",
        ObjectKind::View => "view",
        ObjectKind::Collection => "collection",
        ObjectKind::Keys => "keys",
        ObjectKind::Function => "function",
    }
}

fn reply(value: Value) -> Result<String, String> {
    to_string_pretty(&value).map_err(|error| error.to_string())
}

fn failed(error: anyhow::Error) -> String {
    format!("{error:#}")
}

impl Tabula {
    fn new(
        pool: Pool,
        confirm: mpsc::UnboundedSender<ConfirmRequest>,
        connections: Connections,
    ) -> Self {
        Self {
            tool_router: Self::tool_router(),
            pool,
            confirm,
            connections,
        }
    }

    /// A saved connection by id or name (case-insensitive), with its
    /// secrets from the keychain.
    async fn config(&self, connection: &str) -> Result<ConnectionConfig> {
        let wanted = connection.trim().to_string();
        let mut cfg = (self.connections)()
            .into_iter()
            .find(|cfg| cfg.id == wanted || cfg.name.eq_ignore_ascii_case(&wanted))
            .ok_or_else(|| {
                anyhow!("no saved connection called {wanted:?}; see list_connections")
            })?;
        cfg = spawn_blocking(move || store::load_secrets(&mut cfg).map(|()| cfg)).await??;
        Ok(cfg)
    }

    /// The open connection for `cfg` (in `database`), connecting once.
    async fn open(
        &self,
        cfg: &ConnectionConfig,
        database: Option<&str>,
    ) -> Result<Arc<Connection>> {
        let database = database.map(str::trim).filter(|name| !name.is_empty());
        let key = format!("{}|{}", cfg.id, database.unwrap_or_default());
        let mut pool = self.pool.lock().await;
        if let Some(connection) = pool.get(&key) {
            return Ok(connection.clone());
        }
        let mut connection = Connection::connect(cfg).await?;
        if let Some(database) = database {
            connection.use_database(database).await?;
        }
        let connection = Arc::new(connection);
        pool.insert(key, connection.clone());
        Ok(connection)
    }

    async fn object(
        &self,
        connection: &Connection,
        name: &str,
        schema: Option<&str>,
    ) -> Result<DbObject> {
        let objects = connection.list_objects().await?;
        objects
            .iter()
            .find(|object| {
                object.name == name && (schema.is_none() || object.schema.as_deref() == schema)
            })
            .or_else(|| {
                objects
                    .iter()
                    .find(|object| object.name.eq_ignore_ascii_case(name))
            })
            .cloned()
            .ok_or_else(|| {
                anyhow!("no table, view or collection called {name:?}; see list_objects")
            })
    }

    /// Safe mode, as in the UI: run, refuse, or ask in the app first.
    async fn allow(
        &self,
        cfg: &ConnectionConfig,
        gate: Gate,
        what: &str,
        statements: String,
    ) -> Result<()> {
        match gate {
            Gate::Run => Ok(()),
            Gate::Block(reason) => bail!(reason),
            Gate::Confirm => {
                let (reply, answer) = oneshot::channel();
                self.confirm
                    .send(ConfirmRequest {
                        cfg: cfg.clone(),
                        what: what.to_string(),
                        statements,
                        reply,
                    })
                    .map_err(|_| anyhow!("Tabula can't show the confirmation right now"))?;
                match timeout(CONFIRM_WAIT, answer).await {
                    Ok(Ok(true)) => Ok(()),
                    Ok(_) => bail!("cancelled in Tabula (this connection's safe mode asks first)"),
                    Err(_) => bail!("nobody confirmed it in Tabula within 5 minutes"),
                }
            }
        }
    }
}

#[tool_router]
impl Tabula {
    #[tool(
        description = "The saved connections: name, id, engine, host, database, environment tag and safe mode. Passwords are never included."
    )]
    async fn list_connections(&self) -> Result<String, String> {
        let connections: Vec<Value> = (self.connections)()
            .iter()
            .map(|cfg| {
                json!({
                    "name": cfg.name,
                    "id": cfg.id,
                    "engine": cfg.driver.label(),
                    "host": if cfg.file.is_empty() { format!("{}:{}", cfg.host, cfg.port) } else { cfg.file.clone() },
                    "database": cfg.database,
                    "environment": format!("{:?}", cfg.tag),
                    "safe_mode": format!("{:?}", cfg.safety()),
                })
            })
            .collect();
        reply(Value::Array(connections))
    }

    #[tool(description = "The databases on a connection (Redis: 0–15).")]
    async fn list_databases(
        &self,
        Parameters(args): Parameters<ConnectionArgs>,
    ) -> Result<String, String> {
        async {
            let cfg = self.config(&args.connection).await?;
            let connection = self.open(&cfg, args.database.as_deref()).await?;
            let databases = connection.databases().await?;
            Ok(json!({ "current": connection.current_database(), "databases": databases }))
        }
        .await
        .map_err(failed)
        .and_then(reply)
    }

    #[tool(
        description = "Tables, views, collections, Redis key namespaces and stored functions in a database."
    )]
    async fn list_objects(
        &self,
        Parameters(args): Parameters<ConnectionArgs>,
    ) -> Result<String, String> {
        async {
            let cfg = self.config(&args.connection).await?;
            let connection = self.open(&cfg, args.database.as_deref()).await?;
            let objects: Vec<Value> = connection
                .list_objects()
                .await?
                .iter()
                .map(|object| json!({ "name": object.name, "schema": object.schema, "kind": kind_name(object.kind) }))
                .collect();
            Ok(Value::Array(objects))
        }
        .await
        .map_err(failed)
        .and_then(reply)
    }

    #[tool(
        description = "A table's structure: columns (type, NULL, default, primary key, comment), indexes, foreign keys, tables referencing it, and its CREATE statement. For a function, its definition."
    )]
    async fn describe_object(
        &self,
        Parameters(args): Parameters<ObjectArgs>,
    ) -> Result<String, String> {
        async {
            let cfg = self.config(&args.connection).await?;
            let connection = self.open(&cfg, args.database.as_deref()).await?;
            let object = self
                .object(&connection, &args.object, args.schema.as_deref())
                .await?;
            if object.kind == ObjectKind::Function {
                return Ok(json!({ "definition": connection.routine_definition(&object).await? }));
            }
            let structure = connection.structure(&object).await?;
            let columns: Vec<Value> = structure
                .columns
                .iter()
                .map(|column| {
                    json!({
                        "name": column.name,
                        "type": column.data_type,
                        "nullable": column.nullable,
                        "default": column.default,
                        "primary_key": column.is_pk,
                        "enum_values": column.enum_values,
                        "comment": column.comment,
                    })
                })
                .collect();
            let key = |key: &crate::db::ForeignKey| {
                json!({
                    "name": key.name,
                    "columns": key.columns,
                    "table": key.ref_table,
                    "schema": key.ref_schema,
                    "table_columns": key.ref_columns,
                    "on_delete": key.on_delete,
                })
            };
            Ok(json!({
                "object": object.title(),
                "kind": kind_name(object.kind),
                "columns": columns,
                "indexes": rows_json(&structure.indexes)["rows"],
                "foreign_keys": structure.foreign_keys.iter().map(key).collect::<Vec<_>>(),
                "referenced_by": structure.referenced_by.iter().map(key).collect::<Vec<_>>(),
                "ddl": structure.ddl,
                "warnings": structure.warnings,
            }))
        }
        .await
        .map_err(failed)
        .and_then(reply)
    }

    #[tool(
        description = "Rows of a table, view or collection, with filters, sort and paging (like the data grid). Returns columns, rows and the total count."
    )]
    async fn read_rows(&self, Parameters(args): Parameters<ReadArgs>) -> Result<String, String> {
        async {
            let cfg = self.config(&args.connection).await?;
            let connection = self.open(&cfg, args.database.as_deref()).await?;
            let object = self
                .object(&connection, &args.object, args.schema.as_deref())
                .await?;
            let filters = args
                .filters
                .iter()
                .map(|filter| {
                    Ok(Filter {
                        column: filter.column.clone(),
                        op: filter_op(&filter.op)?,
                        value: filter.value.clone(),
                        value2: filter.value2.clone(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let query = TableQuery {
                object,
                filters,
                match_any: args.match_any,
                order: args
                    .order_by
                    .clone()
                    .map(|column| (column, !args.descending)),
                limit: args.limit.unwrap_or(100).clamp(1, ROW_CAP),
                offset: args.offset,
                find: args.find.clone().unwrap_or_default(),
                fields: args.fields.clone(),
            };
            let (result, total) = connection.fetch_table(&query).await?;
            let mut page = rows_json(&result);
            page["total"] = json!(total);
            Ok(page)
        }
        .await
        .map_err(failed)
        .and_then(reply)
    }

    #[tool(
        description = "Runs SQL (several statements allowed), Redis commands (one per line) or MongoDB shell calls, like the query editor. Writes follow the connection's safe mode: read-only connections refuse them, and 'confirm' modes ask in the Tabula window first."
    )]
    async fn run_query(&self, Parameters(args): Parameters<QueryArgs>) -> Result<String, String> {
        async {
            let cfg = self.config(&args.connection).await?;
            self.allow(
                &cfg,
                gate_query(&cfg, &args.query),
                "Run query",
                args.query.clone(),
            )
            .await?;
            let connection = self.open(&cfg, args.database.as_deref()).await?;
            let results = connection
                .execute_cancellable(&args.query, &Cancel::default())
                .await?;
            Ok(Value::Array(
                results
                    .iter()
                    .map(|result| {
                        let mut shown = rows_json(result);
                        shown["statement"] = json!(result.statement);
                        shown
                    })
                    .collect(),
            ))
        }
        .await
        .map_err(failed)
        .and_then(reply)
    }

    #[tool(
        description = "Inserts, updates or deletes rows of one table or collection in one transaction, like committing staged edits in the grid. Follows the connection's safe mode."
    )]
    async fn change_rows(
        &self,
        Parameters(args): Parameters<ChangeRowsArgs>,
    ) -> Result<String, String> {
        async {
            let cfg = self.config(&args.connection).await?;
            let connection = self.open(&cfg, args.database.as_deref()).await?;
            let object = self
                .object(&connection, &args.object, args.schema.as_deref())
                .await?;
            let changes = args
                .changes
                .iter()
                .map(|change| {
                    Ok(match change.action.trim().to_ascii_lowercase().as_str() {
                        "insert" => Change::Insert {
                            values: cells(&change.values),
                        },
                        "update" if !change.key.is_empty() => Change::Update {
                            key: cells(&change.key),
                            set: cells(&change.values),
                        },
                        "delete" if !change.key.is_empty() => Change::Delete {
                            key: cells(&change.key),
                        },
                        "update" | "delete" => bail!("{} needs the row's key", change.action),
                        other => bail!("unknown action {other:?}: insert, update or delete"),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            if changes.is_empty() {
                bail!("no changes given");
            }
            let structure = connection.structure(&object).await?;
            let preview = connection.preview(&object, &structure.columns, &changes);
            self.allow(&cfg, gate_write(&cfg), "Save changes", preview.clone())
                .await?;
            connection
                .commit(&object, &structure.columns, &changes)
                .await?;
            Ok(json!({ "applied": changes.len(), "statements": preview }))
        }
        .await
        .map_err(failed)
        .and_then(reply)
    }

    #[tool(
        description = "Sessions on the server (the process list): id, user, state, running time, query."
    )]
    async fn list_processes(
        &self,
        Parameters(args): Parameters<ConnectionArgs>,
    ) -> Result<String, String> {
        async {
            let cfg = self.config(&args.connection).await?;
            let connection = self.open(&cfg, args.database.as_deref()).await?;
            Ok(rows_json(&connection.processes().await?))
        }
        .await
        .map_err(failed)
        .and_then(reply)
    }

    #[tool(
        description = "Cancels a session's running statement, or ends the whole session. Follows the connection's safe mode."
    )]
    async fn kill_process(&self, Parameters(args): Parameters<KillArgs>) -> Result<String, String> {
        async {
            let cfg = self.config(&args.connection).await?;
            let what = if args.whole_session {
                "Kill session"
            } else {
                "Cancel query"
            };
            self.allow(&cfg, gate_write(&cfg), what, format!("{what} {}", args.id))
                .await?;
            let connection = self.open(&cfg, None).await?;
            connection.kill(&args.id, args.whole_session).await?;
            Ok(json!({ "done": what, "id": args.id }))
        }
        .await
        .map_err(failed)
        .and_then(reply)
    }

    #[tool(description = "A Redis key: type, TTL, size and its first 500 elements.")]
    async fn redis_key(&self, Parameters(args): Parameters<KeyArgs>) -> Result<String, String> {
        async {
            let cfg = self.config(&args.connection).await?;
            let connection = self.open(&cfg, args.database.as_deref()).await?;
            let detail = connection.key_detail(&args.key).await?;
            Ok(json!({
                "key": detail.key,
                "type": detail.kind,
                "ttl_seconds": detail.ttl,
                "size_bytes": detail.size,
                "length": detail.length,
                "items": detail.items,
                "truncated": detail.truncated,
            }))
        }
        .await
        .map_err(failed)
        .and_then(reply)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Tabula {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("tabula", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Tabula's saved database connections (PostgreSQL, MySQL, SQLite, Redis, MongoDB). \
                 Start with list_connections, then list_objects / describe_object / read_rows. \
                 Writes follow each connection's safe mode and may wait for the user to confirm in Tabula.",
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Driver, SafeMode};
    use rmcp::ServiceExt;
    use rmcp::model::{CallToolRequestParams, ClientConfig};
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use std::env::temp_dir;
    use std::fs::{remove_file, write};

    #[test]
    fn compares_tokens() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"abcd"));
    }

    #[test]
    fn reads_filter_ops() {
        assert_eq!(filter_op("starts_with").unwrap(), FilterOp::StartsWith);
        assert_eq!(filter_op(">=").unwrap(), FilterOp::Ge);
        assert!(filter_op("near").is_err());
    }

    /// A tool call's text, and whether it was an error.
    async fn call(
        client: &rmcp::service::RunningService<rmcp::RoleClient, ClientConfig>,
        tool: &'static str,
        arguments: Value,
    ) -> (String, bool) {
        let Value::Object(arguments) = arguments else {
            panic!("arguments are an object")
        };
        let result = client
            .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
            .await
            .unwrap();
        let text = result
            .content
            .iter()
            .filter_map(|content| content.as_text().map(|text| text.text.clone()))
            .collect::<String>();
        (text, result.is_error == Some(true))
    }

    /// The tools over real HTTP against a SQLite file: a token is needed,
    /// reads work, read-only refuses writes, "confirm" waits for the app.
    #[tokio::test]
    async fn tools_over_http() {
        let mut shop = ConnectionConfig::new(Driver::Sqlite);
        shop.name = "Shop".into();
        let file = temp_dir().join(format!("tabula-mcp-{}.db", shop.id));
        write(&file, b"").unwrap();
        shop.file = file.to_string_lossy().into();
        let setup = Connection::connect(&shop).await.unwrap();
        setup
            .execute_cancellable(
                "create table orders (id integer primary key, status text); \
                 insert into orders values (1, 'paid'), (2, 'pending')",
                &Cancel::default(),
            )
            .await
            .unwrap();
        let mut locked = shop.clone();
        locked.id = format!("{}-ro", shop.id);
        locked.name = "Shop read-only".into();
        locked.safe_mode = SafeMode::ReadOnly;
        let mut careful = shop.clone();
        careful.id = format!("{}-confirm", shop.id);
        careful.name = "Shop careful".into();
        careful.safe_mode = SafeMode::ConfirmWrites;
        let saved = vec![shop.clone(), locked, careful];
        let (confirm, mut asks) = mpsc::unbounded_channel();
        let token = new_token();
        let server = start(0, token.clone(), confirm, Arc::new(move || saved.clone()))
            .await
            .unwrap();
        let address = url(server.port);

        // Without the token: refused before MCP even starts.
        let anonymous = ClientConfig::default()
            .serve(StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(address.clone()),
            ))
            .await;
        assert!(anonymous.is_err(), "no token, no access");

        let client = ClientConfig::default()
            .serve(StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(address).auth_header(token),
            ))
            .await
            .unwrap();
        let (listed, _) = call(&client, "list_connections", json!({})).await;
        assert!(
            listed.contains("Shop read-only") && !listed.contains("password"),
            "{listed}"
        );
        let (objects, _) = call(&client, "list_objects", json!({ "connection": "shop" })).await;
        assert!(objects.contains("orders"), "{objects}");
        let (described, _) = call(
            &client,
            "describe_object",
            json!({ "connection": "Shop", "object": "orders" }),
        )
        .await;
        assert!(described.contains("\"primary_key\": true"), "{described}");
        let (rows, _) = call(
            &client,
            "read_rows",
            json!({ "connection": "Shop", "object": "orders",
                    "filters": [{ "column": "status", "op": "eq", "value": "paid" }] }),
        )
        .await;
        assert!(
            rows.contains("\"total\": 1") && rows.contains("paid"),
            "{rows}"
        );
        let (changed, error) = call(
            &client,
            "change_rows",
            json!({ "connection": "Shop", "object": "orders", "changes": [
                { "action": "update", "key": { "id": 2 }, "values": { "status": "shipped" } },
                { "action": "insert", "values": { "id": 3, "status": "new" } } ] }),
        )
        .await;
        assert!(!error, "{changed}");
        let (counted, _) = call(
            &client,
            "run_query",
            json!({ "connection": "Shop", "query": "select count(*) as n from orders where status in ('shipped', 'new')" }),
        )
        .await;
        assert!(counted.contains("\"n\": \"2\""), "{counted}");

        // Read-only: writes refused, reads fine.
        let (refused, error) = call(
            &client,
            "run_query",
            json!({ "connection": "Shop read-only", "query": "delete from orders" }),
        )
        .await;
        assert!(error && refused.contains("read-only"), "{refused}");

        // Confirm writes: the app is asked; a "no" cancels, a "yes" runs.
        let answer_no = tokio::spawn(async move {
            let ask = asks.recv().await.unwrap();
            assert_eq!(ask.what, "Run query");
            ask.reply.send(false).unwrap();
            asks
        });
        let (declined, error) = call(
            &client,
            "run_query",
            json!({ "connection": "Shop careful", "query": "delete from orders" }),
        )
        .await;
        assert!(error && declined.contains("cancelled"), "{declined}");
        let mut asks = answer_no.await.unwrap();
        tokio::spawn(async move { asks.recv().await.unwrap().reply.send(true).unwrap() });
        let (_, error) = call(
            &client,
            "run_query",
            json!({ "connection": "Shop careful", "query": "delete from orders where id = 3" }),
        )
        .await;
        assert!(!error);

        let _ = client.cancel().await;
        drop(server);
        let _ = remove_file(file);
    }
}
