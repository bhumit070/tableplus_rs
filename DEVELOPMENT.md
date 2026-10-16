# Development

## Architecture

```
crates/
  db-core/       Backend-agnostic types + traits. No I/O, no SQL driver deps.
  db-postgres/    PostgreSQL adapter (sqlx, rustls)
  db-mysql/       MySQL adapter      (sqlx, rustls)
  db-mssql/       SQL Server adapter (tiberius, rustls)
  sql-intel/      SQL tokenizing, syntax-highlight classification, autocomplete
  app/            The GUI (binary: `tableplus-rs`), built with `iced`
```

### `db-core`

Defines everything the rest of the workspace shares, so no other crate needs to know which backend produced a value:

- `Value` — one enum covering every cell type across all three backends (`Null`, `Bool`, `I64`, `F64`, `Decimal`, `Text`, `Bytes`, `Date`/`Time`/`DateTime`/`TimestampTz`, `Uuid`, `Json`, `Array`).
- `QueryResult` / `Column` — the result of running one statement.
- `Catalog` / `DatabaseInfo` / `SchemaInfo` / `TableInfo` / `ColumnInfo` / `ForeignKeyInfo` / `IndexInfo` — normalized schema introspection, used by both the sidebar tree and the autocomplete engine.
- `ConnectionConfig` / `DriverKind` / `SslMode` — everything needed to open a connection *except the password*, which is resolved separately from the OS keychain (see `app/src/store/secrets.rs`) so a `ConnectionConfig` is always safe to serialize/export.
- `DbAdapter` — the async trait every backend implements: `execute`, `fetch_catalog`, `bulk_insert`, `cancel`, `close`. The `app` crate holds connections as `Box<dyn DbAdapter>` / `Arc<dyn DbAdapter>`.
- `export` / `import` — pure data transforms (`QueryResult` → CSV/JSON/SQL text; CSV/JSON text → typed rows), independent of any backend.
- `connection_set` — export/import of the *list* of saved connections (distinct from exporting table/query *data*).

### Adapter crates

Each of `db-postgres`, `db-mysql`, `db-mssql` follows the same shape:

- `connect(config, password) -> impl DbAdapter` — opens **one physical connection** (not a pool). A desktop DB client runs one statement at a time per open connection tab, and a single connection is what makes `cancel()` simple and correct: a second, short-lived connection issues the backend's native cancel command against the first connection's session id (`pg_cancel_backend`, `KILL QUERY`, `KILL`).
- `convert.rs` — decodes the driver's native row type into `db_core::Value`, and binds `Value` back into parameterized `INSERT` statements for `bulk_insert`.
- `catalog.rs` — introspection queries against `information_schema` (plus `pg_catalog`/`sys.*` for indexes, which aren't in the SQL standard).
- `tests/integration.rs` — runs against the real container from `docker-compose.yml` (see below). Connection settings are overridable via env vars; sensible defaults point at the compose file's ports so `docker compose up -d && cargo test` works with no extra setup.

**Adding a fourth backend** means: add a crate following this shape, implement `DbAdapter`, add it to `app/src/connect.rs`'s dispatch, add a `DriverKind` variant in `db-core`, and add a service + init script to `docker-compose.yml`.

### `sql-intel`

Framework-agnostic (no `iced` dependency) so it could back a different GUI, a CLI, or an LSP later.

- `dialect.rs` — maps `DriverKind` to the matching `sqlparser` dialect (Postgres/MySQL/MSSQL quoting and keyword differences).
- `highlight.rs` — tokenizes a string and classifies each token (`Keyword`/`Identifier`/`StringLiteral`/.../`Comment`) with byte-offset spans. Never fails: a syntactically incomplete buffer (the normal state of an editor mid-keystroke) still highlights whatever tokenized cleanly.
- `complete.rs` — context-aware autocomplete. **This does not require a syntactically complete statement.** It tokenizes just the text up to the cursor and uses a small set of heuristics:
  - nearest preceding keyword decides the "position" (`FROM`/`JOIN`/`INTO`/`UPDATE`/`TABLE` → suggest tables; `SELECT`/`WHERE`/... → suggest columns/functions/clause keywords; nothing yet → suggest statement keywords)
  - a lightweight scan for `FROM <table> [AS] <alias>` / `JOIN ...` builds an alias→table map, so `t.` after `FROM customers t` suggests `customers`' columns specifically.

  This is the same category of approach most lightweight SQL editors use. A full AST-based resolver (parse the whole statement once it's syntactically valid, walk the real query plan) is the natural next step — see [Roadmap](#roadmap).

### `app`

An `iced` 0.14 application (functional `boot`/`update`/`view`, not the old `Application` trait).

- `model.rs` — `App` state: saved connections, open `Session`s (one per connected `ConnectionConfig`, holding its `Arc<dyn DbAdapter>` and fetched `Catalog`), open `Tab`s (one per query editor), and the connection dialog state.
- `message.rs` — the `Message` enum. Async work (connecting, running a query, fetching a catalog, file dialogs) is modeled as `Task::perform(future, Message::Variant)`, following iced's standard pattern.
- `connect.rs` — the one place that matches on `DriverKind` and calls into the right adapter crate.
- `store/` — connection list persistence (JSON file under the OS config dir via the `directories` crate) and OS-keychain password access (via the `keyring` crate).
- `editor/`
  - `highlighter.rs` — implements iced's per-line `Highlighter` trait on top of `sql_intel::highlight`, so the editor's syntax colors update live and automatically match the active connection's dialect.
  - `autopair.rs` — intercepts `TextEditor::key_binding` to turn "insert an opening bracket/quote" into "insert the pair and put the cursor in between."
  - `cursor.rs` — converts iced's line/column cursor position into a byte offset into the full buffer text (what `sql_intel::complete` needs).
- `view/` — `sidebar.rs` (connections + schema tree), `connection_dialog.rs`, `query_tab.rs` (editor + toolbar + results), `grid.rs` (results table).

## Setting up a dev environment

```sh
rustup toolchain install stable
docker compose up -d
docker compose logs -f mssql-init   # wait for "mssql sample schema ready"
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

`docker-compose.yml` starts:

- `postgres` (16, Alpine) on `localhost:55432`
- `mysql` (8.4) on `localhost:53306`
- `mssql` (SQL Server 2022) on `localhost:51433`, plus a `mssql-init` sidecar that applies `docker/init/mssql/*.sql` once the server is healthy (the official SQL Server image, unlike the Postgres/MySQL ones, has no init-script hook of its own).

All three are seeded with the same sample schema: a `shop` schema (or, for MySQL, a `tableplus_rs_sample` database — MySQL has no schema/database distinction) containing `customers` and `orders` tables with a foreign key and an index, so the adapters' introspection code has real constraints/indexes to find.

Each adapter's `tests/integration.rs` connects using env-var-overridable defaults that match the compose file, so `docker compose up -d && cargo test --workspace` works with zero additional configuration. Override with `TABLEPLUS_RS_POSTGRES_{HOST,PORT,USER,PASSWORD,DATABASE}` / `_MYSQL_` / `_MSSQL_` if you're pointing at something else.

### Apple Silicon note

The official `mcr.microsoft.com/mssql/server` image is `linux/amd64` only; Docker Desktop runs it under emulation on Apple Silicon. It works, but connects/queries are noticeably slower than the other two containers — this is a Docker/QEMU limitation, not an adapter issue.

### Linux notes

- `rfd` (file dialogs) is built with the `xdg-portal` feature to avoid a hard GTK3 build dependency; it talks to the desktop's `org.freedesktop.portal.FileChooser` service. On a desktop environment without a portal implementation, dialogs won't appear — this is a known rough edge, see Roadmap.
- `keyring` needs a Secret Service provider (GNOME Keyring, KWallet via its Secret Service shim, etc.) running. Headless/minimal Linux installs may need one installed.

## Testing

- `cargo test -p db-core` — pure logic (export/import transforms, connection-set roundtrip). No network/DB required.
- `cargo test -p sql-intel` — tokenizing, highlighting, autocomplete heuristics. No network/DB required.
- `cargo test -p db-postgres` / `-p db-mysql` / `-p db-mssql` — integration tests against the live containers from `docker-compose.yml`.
- `cargo test -p tableplus-rs` — a handful of pure-logic unit tests for the editor (cursor byte-offset math, auto-pair matching). The GUI itself isn't under automated test; see Roadmap.

## Code style

- `cargo clippy --workspace --all-targets -- -D warnings` must be clean. `redundant_clone`, `needless_collect`, and `large_enum_variant` are promoted to warnings workspace-wide in the root `Cargo.toml`.
- No `unwrap()`/`expect()` outside of tests. Adapter errors funnel through `db_core::DbError`; the `app` crate uses `anyhow` only at its own boundaries (file I/O, keychain, config parsing).
- Comments explain *why*, not *what* — see any adapter's `cancel()` for the pattern (a one-paragraph note on why cancellation needs a second connection, not a restatement of the code).

## Roadmap

Rough edges and intentionally-deferred work, roughly in priority order:

- **Autocomplete**: suggestions render in a panel below the editor, not a floating popup anchored at the text cursor (iced has no built-in overlay widget for this; it's buildable with `iced::advanced::overlay` but is a sizable effort on its own). The completion engine itself is heuristic/token-based rather than a full AST resolver.
- **Results grid**: not virtualized — every row becomes real widgets. Fine for the result sets a human is actually looking at; will get slow on a result set in the hundreds of thousands of rows. Needs either `iced::widget::canvas` or a custom scroll-aware widget.
- **MSSQL auth**: SQL authentication only — no Windows Integrated Auth (would require platform-specific native libraries, which conflicts with the "install nothing" goal). Custom CA certs for `SslMode::VerifyFull` aren't wired through yet (tracked as `TODO(#1)` in `db-mssql/src/lib.rs`); self-signed certs are trusted unconditionally in the meantime.
- **No query history / saved queries / multiple result tabs per query** yet.
- **No table-structure editor** (create/alter table, column/index editing via UI) — only SQL and CSV/JSON data import today.
- **No light theme** — the editor's syntax colors and the app theme are both fixed dark-mode values currently.
- **No packaging** — no platform installers/bundling (`.app`, `.msi`, AppImage/`.deb`) or CI pipeline yet, only `cargo build`.
- **GUI isn't under automated/snapshot test** — only the pure-logic pieces (cursor math, auto-pair rules) are. Manual smoke-testing (`cargo run -p tableplus-rs`) is the current verification path for view/update wiring.
