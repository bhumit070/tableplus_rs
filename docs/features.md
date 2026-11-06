# Features

## Connections

- A saved list with **environment tags** (local, development, testing,
  staging, production), shown as colored badges.
- **No connection limit.** Each open connection gets an icon in the left
  rail; ⌘1…⌘9 jump between them.
- **Session restore:** last time's connections and tabs, including unsaved
  query text, reopen at launch (can be turned off in Settings).
- **SSH tunnels** with password, private key (with passphrase) or
  ssh-agent. Host keys are checked; a dead tunnel is reopened automatically.
- **Import from TablePlus**, including passwords from export files.

## Browsing and editing data

- **Table view:** a spreadsheet grid with paging, sorting and a filter bar
  (⌘F). Several filters combine with AND.
- **Staged editing:** edits are colored (yellow edited, green new, red
  deleted) and nothing is written until you commit (⌘S). *Preview SQL*
  shows exactly what will run. Commits are one transaction where the
  database supports it.
- **Inline cell editing:** Enter or typing starts editing, Tab / ⇧Tab move
  between cells, Esc cancels. Multi-line values open a larger editor.
  Enum columns (PostgreSQL and MySQL) open a dropdown of their values,
  plus NULL when the column allows it.
- **Row detail panel** (⌘⇧I): every field of the selected row, with JSON
  pretty-printed.
- **Foreign keys:** a → in a foreign-key cell opens the referenced row.
- **Structure view:** columns, indexes, foreign keys and the table's
  `CREATE TABLE` statement.
- **New Table / Edit Columns:** add, rename, retype or drop columns, change
  NULL / default / primary key. The DDL is shown before it runs.
- **Sidebar actions:** open, copy name, new query, rename, duplicate,
  vacuum / optimize, truncate and drop.
- **PostgreSQL schemas:** tables are grouped by schema, and the Schema
  picker sets `search_path` so unqualified names find the chosen schema.
- **Redis:** keys grouped by namespace (`user:*`), with type, TTL and value.
- **MongoDB:** collections shown as tables, with columns from the union of
  top-level fields.

## Query editor

- Syntax highlighting, and **autocomplete** for tables, columns (including
  aliases), keywords and functions. Redis and MongoDB get command and
  method names.
- ⌘↵ runs the selection, or the whole editor. **One result tab per
  statement.**
- **Stop** (⌘.) cancels the running statement on the server
  (`pg_cancel_backend` / `KILL QUERY`), not just in the app.
- **Editable results:** a single-table `SELECT` can be edited like a table.
- **SQL formatter** (⌘⇧F), **query history** (⌘Y), **saved queries**
  (⌘⇧S / ⌘⇧O) for one connection or all of them.
- **EXPLAIN** (⌘E) as a plan tree with cost, rows and a bar per step.
  *Analyze* shows actual times; it runs inside a transaction that is rolled
  back.

## Copy, export and import

- **Copy as** JSON, INSERT, UPDATE, CSV, Markdown, or a column as an
  `IN (…)` list.
- **Export** (⌘⇧E) to CSV, TSV, JSON, JSON Lines, Markdown, HTML or SQL.
  Table exports stream every page to disk, so large tables don't fill
  memory. In CSV, NULL is an empty field and an empty string is `""`.
- **Import CSV** into an existing table: columns are matched by header
  name, and all rows go in one transaction.

## Administration

- **Process list** (⌘⇧P): server sessions, refreshed every few seconds,
  with Cancel Query and Kill Connection.
- **Backup / Restore** with the database's own tools (`pg_dump` / `psql`,
  `mysqldump` / `mysql`); SQLite is copied with `VACUUM INTO`.
- **Create / drop databases** and the **database switcher** (⌘K).

## Safety

- **Safe mode** per connection: automatic (asks before writes on
  *production*), off, confirm writes, confirm every query, or **read-only**.
  Read-only is also enforced by the database session, not only by the app.
  See [Safety and security](architecture/safety.md).

## Settings (⌘,)

Interface and editor font sizes, session restore, and light / dark theme
(⌘⇧L).

## Not yet supported

ER diagrams, users & roles, schema compare, an AI assistant, more database
drivers, and multiple windows.
