# Database layer

Everything that talks to a server, or needs to understand SQL, is in
`src/db/`. The UI only sees `db::Connection` and a few plain data types.

## `Connection`: one type, five drivers

```rust
pub struct Connection {
    inner: Conn,                    // Sql | Mssql | Redis | Mongo
    _tunnel: Option<ssh::Tunnel>,   // closes when the connection is dropped
    endpoint: (String, u16),        // where the driver actually connects
}
```

`Connection::connect(&ConnectionConfig)` opens an SSH tunnel first if one
is configured. The driver then connects to the tunnel's local port instead
of the real host. Every other method delegates to the driver:

| Method | What it does |
| --- | --- |
| `list_objects` | Tables, views, collections or Redis key groups for the sidebar. |
| `all_columns` | Every (schema, table, column), for autocomplete. |
| `databases`, `use_database`, `set_schema` | ⌘K switcher and the Postgres schema picker. |
| `structure` | Columns, indexes, foreign keys and DDL for the Structure view. |
| `fetch_table(TableQuery)` | One page of rows with filters and sort, plus the total count. |
| `execute_cancellable(text, &Cancel)` | Runs editor text; one `QueryResult` per statement. |
| `preview` / `commit(changes)` | Shows / runs the staged grid edits. |
| `processes`, `kill` | Process list, cancel query, kill connection. |
| `create_database`, `drop_database`, `rename_object`, `duplicate_table`, `truncate`, `drop_object`, `maintain` | Admin actions. |

Background reads (`list_objects`, `all_columns`, `databases`, `structure`,
`processes`, `fetch_table`) go through `bounded()`, which gives up after
30 seconds and explains why (server down or busy, connection lost, tunnel
dropped). Editor queries have no timeout and are stopped with Cancel.

### The shared data types

| Type | Meaning |
| --- | --- |
| `ConnectionConfig` | A saved connection: driver, host, port, user, database or file, TLS, SSH, environment tag, safe mode. `password` is filled from the keychain, never serialized. |
| `DbObject` | `{schema, name, kind}`, where kind is table, view, collection or key space. |
| `QueryResult` | `columns`, `rows: Vec<Vec<Option<String>>>`, `affected`, `elapsed_ms`, and the `statement` it came from. |
| `TableQuery` | Object, filters (`column op value`), order, limit, offset. |
| `Change` | A staged edit: `Update {key, set}`, `Insert {values}`, `Delete {key}`. The key is the primary key for SQL, `_id` for MongoDB, the key name for Redis. |
| `Structure` | Columns, indexes, foreign keys, DDL, plus warnings for the parts that couldn't be loaded. |

## SQL: PostgreSQL, MySQL, SQLite (`sql.rs`)

All three use [sqlx](https://github.com/launchbadge/sqlx) with a small pool
(4 connections, rustls for TLS).

- **Connecting:** before creating the pool, `probe()` makes one direct
  connection with a 15-second timeout. A pool's own errors are often just
  "timed out", but the probe shows the server's real answer, such as a wrong
  password, an unknown database or a refused TLS connection.
- **Reading:** reads go through `raw_sql`, sqlx's simple/text protocol.
  Every value arrives as the server's text, so any column type can be shown
  without a decoder for it.
- **Writing:** grid commits are built as statements with bound parameters
  and run in one transaction (`run_tx!`). Identifiers are quoted for each
  dialect.
- **Editor batches** (`run_batch!`) run every statement of a batch **on one
  pooled connection**, so `BEGIN … ROLLBACK`, `SET` and temporary tables
  apply to the statements after them. Afterwards, `SessionEffects` decides
  what to do with that connection:
    - a transaction left open is rolled back, and the result says so;
    - a connection whose session state changed (`SET`, `USE`, temp tables,
      `PRAGMA x = …`) or that failed is closed instead of going back to the
      pool, where it would leak that state into later reads.
- **Read-only safe mode** is also set on the server for every pooled
  connection:
    - Postgres: `set session characteristics as transaction read only`
    - MySQL: `set session transaction read only`
    - SQLite: the file is opened read-only.

### Cancel (`cancel.rs`)

A `Cancel` is created for each editor run. Before the batch starts, the
driver registers how to cancel it **on the server**: it looks up its own
session id (`pg_backend_pid()` or `connection_id()`) and stores a closure
that runs `pg_cancel_backend(id)` or `KILL QUERY id` from another pooled
connection. ⌘. then:

1. runs that server-side cancel (waiting at most 5 seconds);
2. wakes the batch's `tokio::select!`, which stops waiting;
3. drops the connection the statement ran on.

A statement that finishes after the cancel was requested still counts as
cancelled. (MySQL's `SLEEP` returns 1 when killed, as if it finished.) If
the server-side cancel fails, the message says the server may still be
running the statement and points to the process list.

### SQL helpers

| File | Purpose |
| --- | --- |
| `split.rs` | Splits editor text on `;`, except inside quotes, comments and Postgres dollar-quoted bodies. MySQL backslash escapes are handled. |
| `safety.rs` | Decides whether text only reads (see [Safety](safety.md)). |
| `format.rs` | The SQL formatter: upper-case keywords, one clause per line, indented subqueries. Strings, quoted names and comments stay exactly as written. |
| `target.rs` | Finds the single table a `SELECT` reads from, so its result can be edited. |
| `ddl.rs` | `CREATE TABLE` / `ALTER TABLE` for the table editor, per dialect. SQLite only supports add, rename and drop column. SQL Server renames with `sp_rename`, drops a column's named default first, and writes comments as `MS_Description`; its script puts each statement in its own `GO` batch inside one `XACT_ABORT` transaction. |
| `explain.rs` | EXPLAIN as a tree: Postgres `FORMAT JSON`, MySQL `FORMAT=TREE`, SQLite `EXPLAIN QUERY PLAN`. *Analyze* runs inside `BEGIN … ROLLBACK`. |
| `backup.rs` | Backup / restore through `pg_dump` / `psql` and `mysqldump` / `mysql`, found on `PATH` or in the usual install folders. Passwords go through `PGPASSWORD` or a private (`0600`) MySQL options file that is deleted afterwards, never the command line. SQLite uses `VACUUM INTO` a temporary file, then a rename. A Postgres restore streams the dump into `psql` and turns its `SET` lines for settings added in later versions (`NEWER_SETTINGS`, e.g. 17's `transaction_timeout`) into `set_config` calls applied only when the server has them, outside COPY data, so a dump from a newer pg_dump restores into an older server. Other `SET` lines go through unchanged, so a typo still stops the restore. |

## SQL Server (`mssql.rs`)

sqlx dropped its SQL Server driver, so this one uses
[tiberius](https://github.com/prisma/tiberius) (rustls, TDS 8.0) behind the
same methods as `SqlConn`.

- **Pool:** tiberius has none, so `mssql.rs` keeps up to 4 connections. A
  connection goes back only through `Pooled::release` after a request that
  finished cleanly; dropped any other way (error, timeout, cancel) it is
  closed. One idle for over a minute is pinged before reuse. Each new
  connection sets the SSMS defaults (`QUOTED_IDENTIFIER`, `ANSI_*`,
  unlimited `TEXTSIZE`).
- **Reading:** every `ColumnData` is turned into text in `cell()`: dates in
  ISO form with at most 7 fractional digits (so they compare equal when
  written back), legacy `datetime` rounded to the millisecond, `bit` as
  1 / 0, binary as `0x…`. A statement with no result set gets its count
  from `rowcount_big()`.
- **Writing:** commits use `@Pn` text parameters (the server converts
  them), `convert(varbinary(max), @Pn, 1)` for hex into binary columns, in
  one `XACT_ABORT` transaction. Each statement's count is read with
  `rowcount_big()`, so triggers' own counts don't confuse the one-row
  check.
- **Editor batches** (`split::split_mssql`) run on one connection in a
  spawned task. Cancel goes through `Cancel::on_server_cancel` to that
  task, which drops the statement and sends a TDS attention
  (`cancel_query`); the server confirms before the cancel returns. Like
  the other SQL drivers, a transaction left open (`@@trancount`) is rolled
  back and reported, and a connection whose session changed is closed.
- **Catalog:** `sys.*` views for objects, columns (with identity, computed
  and `MS_Description`), indexes, keys and checks. There is no SHOW CREATE
  TABLE, so `table_ddl` rebuilds it; *Duplicate* reuses it with unnamed
  constraints. SQL Server stores `IN ('a','b')` checks as
  `([c]='b' OR [c]='a')`, which `check_in_lists` reads (reversed back).
- **Read-only** mode has no server setting: `execute` and `commit` refuse
  anything `safety::is_read_only` doesn't read as a read.
- Its futures are large (TLS handshake), so `Connection` boxes them; else
  the test threads' stacks overflow in debug builds.

## Redis (`redis_kv.rs`)

- Keys appear as one *keys* table with `key`, `type`, `ttl` and `value`
  columns. The sidebar groups keys by namespace (`user:*`). `SCAN` stops
  after 10,000 keys so huge keyspaces stay responsive.
- The connection is a redis-rs `ConnectionManager`, which reconnects by
  itself after a server restart. It uses a 15-second connect timeout and a
  30-second response timeout. A plain connection is made first, so an
  unreachable server fails fast with a clear error instead of retrying for
  minutes.
- The editor runs **one command per line**. Blocking commands (`BLPOP`,
  `SUBSCRIBE`, `MONITOR`, …) are refused, because they would hold the
  shared connection.
- Commits run as one pipeline. String values are edited with `SET`, TTLs
  with `EXPIRE` / `PERSIST`, and renames with `RENAMENX`. Commits refuse
  value edits of other types (hash, list, …); change those from the
  editor. A new key
  is created with `SET … NX` (with `EX` when it has a TTL), so it can't
  overwrite a key another client created in the meantime.

## MongoDB (`mongo.rs`)

- Collections are shown as tables. The columns are the union of the
  top-level fields of the sampled documents, with `_id` first.
- Values are shown as relaxed JSON. Edited text is turned back into BSON:
  string fields stay strings, and everything else is parsed as JSON5.
- The editor accepts **mongosh-style calls**, for example
  `db.users.find({age: {$gt: 30}}).sort({name: 1}).limit(10)`, or a raw
  command document.
- Filters map to a query document. Several filters on one field are
  combined with `$and`.
- A commit isn't atomic on a standalone server (no multi-document
  transaction), and `commits_atomically()` tells the UI so.

## Tests against real servers (`it.rs`)

`src/db/it.rs` runs end-to-end checks of every driver. SQLite always runs;
the others run when `TABULA_IT=1` and the test containers are up (see
[Development](../development.md#tests)).
