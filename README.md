# Tabula

A fast, native database client in the spirit of TablePlus, written in Rust with
[gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui) and
[gpui-component](https://github.com/longbridge/gpui-component). Open as many
connections as you like: each one gets an icon in the left rail.

## Supported databases

| Database | Browse & edit | Query editor |
| --- | --- | --- |
| PostgreSQL | ✓ | SQL |
| MySQL / MariaDB | ✓ | SQL |
| SQLite | ✓ | SQL |
| Redis | keys, types, TTLs, values | one command per line |
| MongoDB | collections as tables | mongosh-style: `db.users.find({age: {$gt: 30}})` |

## Features

- **Connections:** saved list with environment tags (local → production) and
  passwords in the macOS Keychain. Last session's connections and tabs
  (including unsaved query text) reopen at launch.
- **SSH tunnels:** password, private key (with passphrase) or ssh-agent.
  Host keys are checked against `~/.ssh/known_hosts`; a new host asks to be
  trusted, a changed key is refused.
- **Safe mode** per connection: automatic (confirm writes on *production*),
  off, confirm writes, confirm every query, or read-only — read-only is also
  enforced by the server session, not just the editor.
- **Import from TablePlus:** password-protected `.tableplusconnection` exports,
  including their database passwords, or TablePlus's own connection list on
  this Mac.
- **Table view:** spreadsheet-style editing. Changes are staged (yellow edited,
  green new, red deleted) until you commit, and *Preview SQL* shows what will
  run. Also filters, paging, sorting, and a **row detail panel** (⌘⇧I) with
  JSON pretty-printed.
- **Foreign keys:** a → in a foreign-key cell opens the referenced row.
- **Structure:** columns, indexes, foreign keys and the table's SQL
  (`CREATE TABLE …`). **New Table** and **Edit Columns** (add, rename,
  retype, NULL, default, drop) preview the DDL before running it.
- **Copy & export:** copy rows as JSON / INSERT / UPDATE, a column as an
  `IN (…)` list, everything as CSV / Markdown / INSERT; export (⌘⇧E) to CSV,
  TSV, JSON, JSON Lines, Markdown, HTML or SQL — table exports include every
  page. **Import CSV** into a table in one transaction.
- **Query editor:** syntax highlighting, one result tab per statement,
  autocomplete for tables, columns (including aliases), keywords and
  functions, **Stop** (⌘.) that cancels on the server, a **SQL formatter**
  (⌘⇧F), **query history** (⌘Y) and **saved queries** (⌘⇧S / ⌘⇧O).
  Single-table results are editable like a table.
- **EXPLAIN** (⌘E) as a plan tree with cost, rows and a bar per step;
  *Analyze* shows actual times (run inside a rolled-back transaction).
- **Admin:** process list with cancel / kill (⌘⇧P), backup / restore
  (`pg_dump` / `psql`, `mysqldump` / `mysql`, SQLite file copy), create /
  drop databases, rename / duplicate / vacuum tables.
- **Redis:** keys grouped by namespace (`user:*`) in the sidebar.
- **Database switcher (⌘K):** jump between databases on the same connection.
- **Schemas (PostgreSQL):** the sidebar groups tables by schema, and the
  Schema dropdown shows one schema and makes it the default for queries
  (`search_path`), so `SELECT * FROM orders` finds `sales.orders`.
- **Settings (⌘,):** interface and editor font sizes, light/dark theme.

## Keyboard shortcuts

On Windows/Linux, use Ctrl in place of ⌘. Shortcuts work wherever focus is.

| Shortcut | Action |
| --- | --- |
| ⌘K | Switch database |
| ⌘T / ⌘W | New query tab / close tab |
| ⌘⇧] / ⌘⇧[ | Next / previous tab |
| ⌘↵ | Run selection, or the whole editor |
| ⌘. | Stop the running query |
| ⌘E | Explain the query |
| ⌘⇧F | Format SQL |
| ⌘Y | Query history |
| ⌘⇧S / ⌘⇧O | Save query / open a saved query |
| ⌘⇧E | Export the table or result |
| ⌘⇧I | Row detail panel |
| ⌘⇧P | Process list |
| ⌘S | Commit staged changes |
| ⌘⇧⌫ | Discard staged changes |
| ⌘R | Refresh table (or the sidebar) |
| ⌘F | Toggle the filter bar (outside text fields) |
| ⌘I / ⌘⌫ | Add row / delete row (⌘⌫ outside text fields) |
| Enter / typing | Edit the selected cell (typing replaces the value) |
| Tab / ⇧Tab | While editing: save and edit the next / previous cell |
| Esc | While editing: cancel |
| Ctrl+Space | Show autocomplete |
| ⌘N or ⌘⇧K | Open a connection: switch to an open one, open a saved one, or add new |
| ⌘1 … ⌘9 | Switch to the 1st … 9th open connection |
| ⌘0 | Connection list |
| ⌘= / ⌘− | Zoom in / out |
| ⌘⇧L | Toggle light/dark theme |

## Releases

Every push to `main` that changes Rust code runs the tests and, if they pass,
publishes a GitHub Release with macOS builds for Apple Silicon and Intel
(`.github/workflows/release.yml`). Versions are `vMAJOR.MINOR.PATCH`:
MAJOR.MINOR come from `Cargo.toml`, and PATCH counts up per release. The
app isn't notarized yet, so on first launch right-click Tabula.app → Open.

## Building

Requires the Rust toolchain pinned in `rust-toolchain.toml` (rustup installs
it automatically).

```sh
cargo run --release
```

On macOS, shaders are compiled at startup (`runtime_shaders`), so Xcode's
Metal Toolchain isn't needed.

Settings and saved connections live in
`~/Library/Application Support/tabula/` (`settings.json`,
`connections.json`, `saved_queries.json`, `history.db`, `session.json`,
`known_hosts`). Passwords are never written there.

## Tests

```sh
cargo test                  # unit tests, SQLite end-to-end, UI keystroke tests
TABULA_IT=1 cargo test      # also runs against real Postgres/MySQL/Redis/MongoDB
```

`TABULA_IT=1` expects throwaway servers on these ports:

```sh
docker run -d --rm --name tabula-test-pg -e POSTGRES_PASSWORD=test -p 55432:5432 postgres:17-alpine
docker run -d --rm --name tabula-test-mysql -e MYSQL_ROOT_PASSWORD=test -e MYSQL_DATABASE=app -p 53306:3306 mysql:8.4
docker run -d --rm --name tabula-test-redis -p 56379:6379 redis:7-alpine
docker run -d --rm --name tabula-test-mongo -p 57017:27017 mongo:7
docker exec tabula-test-pg createdb -U postgres tabula_it_other
```

## Not yet supported

ER diagrams, users & roles, schema compare, an AI assistant, more database
drivers, and multiple windows.
