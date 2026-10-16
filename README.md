# TablePlus-rs

A native, cross-platform (macOS / Windows / Linux) database GUI client written in Rust, inspired by [TablePlus](https://tableplus.com/). It connects to **PostgreSQL**, **MySQL**, and **SQL Server**, and runs as a single native binary — no JVM, no Electron/Chromium, no system-installed database client libraries.

> **Status:** actively developed. The adapters (connect, query, introspect schema, bulk import) are solid and covered by integration tests against real databases. The GUI has a working connection manager, schema browser, SQL editor (with syntax highlighting, auto-pairing, and autocomplete), results grid, and data import/export. See [DEVELOPMENT.md](DEVELOPMENT.md#roadmap) for what's still rough or missing.

## Why no native client libraries?

Every adapter uses a pure-Rust driver:

| Database   | Driver                                   |
|------------|-------------------------------------------|
| PostgreSQL | [`sqlx`](https://github.com/launchbadge/sqlx) (rustls) |
| MySQL      | [`sqlx`](https://github.com/launchbadge/sqlx) (rustls) |
| SQL Server | [`tiberius`](https://github.com/prisma/tiberius) (rustls) |

There's no `libpq`, no `libmysqlclient`, no ODBC driver manager, no `msodbcsql` to install. Clone the repo, `cargo build`, and it runs — on any of the three major desktop platforms.

## Features

- **Connections**: save connections per-database (Postgres/MySQL/SQL Server), with SSL modes. Passwords are stored in the OS keychain (Keychain on macOS, Credential Manager on Windows, Secret Service/kwallet on Linux) — never on disk alongside the connection metadata.
- **Export / Import connections**: back up or move your whole connection list as a JSON file (no passwords included, by design).
- **Schema browser**: sidebar tree of schemas, tables, and views, built from each backend's `information_schema` (plus `pg_catalog`/`sys.*` where needed for indexes and foreign keys).
- **SQL editor**:
  - Dialect-aware syntax highlighting (keywords, identifiers, strings, numbers, comments) that automatically matches the active connection's backend.
  - Auto-closing of `()`, `[]`, `{}`, `'`, `"`, and `` ` `` as you type.
  - Context-aware autocomplete: statement keywords at the start of a query, table names after `FROM`/`JOIN`, columns (scoped to the tables actually referenced) after `SELECT`/`WHERE`/etc., and alias-aware `t.column` completion after a dot.
- **Run queries** against any open connection; results render in a grid with row count and timing; cancel a running query from a second connection (`pg_cancel_backend` / `KILL QUERY` / `KILL`, per backend).
- **Export query results** to CSV, JSON, or portable `INSERT` statements.
- **Import data** into a table from a CSV or JSON file, type-coerced against the table's real column types, inserted inside a transaction.

## Getting started

### Prerequisites

- Rust (stable, 1.78+) — [rustup.rs](https://rustup.rs)
- [Docker](https://www.docker.com/) + Docker Compose, if you want to run the integration tests or try the app against real sample databases without installing Postgres/MySQL/SQL Server yourself.

### Build and run

```sh
cargo build --release
./target/release/tableplus-rs
```

or, for development:

```sh
cargo run -p tableplus-rs
```

### Try it against real databases

```sh
docker compose up -d
# wait for the mssql-init sidecar to finish seeding SQL Server:
docker compose logs -f mssql-init
```

This starts Postgres, MySQL, and SQL Server, each pre-loaded with the same sample `shop` schema (`customers`, `orders`). Connect to them from the app with:

| Driver     | Host      | Port  | User        | Password        | Database              |
|------------|-----------|-------|-------------|-----------------|------------------------|
| PostgreSQL | localhost | 55432 | `tableplus` | `tableplus`     | `tableplus_rs_sample`  |
| MySQL      | localhost | 53306 | `tableplus` | `tableplus`     | `tableplus_rs_sample`  |
| SQL Server | localhost | 51433 | `sa`        | `TablePlus_rs1` | `tableplus_rs_sample`  |

See [DEVELOPMENT.md](DEVELOPMENT.md) for architecture, how the crates fit together, running the test suite, and the project roadmap.

## Supported platforms

Builds and runs on macOS, Windows, and Linux. The GUI is built with [`iced`](https://github.com/iced-rs/iced), a pure-Rust, cross-platform GUI library (no system webview, no bundled browser engine).

## License

MIT
