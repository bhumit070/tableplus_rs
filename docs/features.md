# Features

## Connections

- A saved list with **environment tags** (local, development, staging,
  production), shown as colored badges. Connections saved with the old
  *testing* tag open as development.
- **No connection limit.** Each open connection gets an icon in the left
  rail; ⌘1…⌘9 jump between them.
- **Session restore:** last time's connections and tabs, including unsaved
  query text, reopen at launch (can be turned off in Settings).
- **SSH tunnels** with password, private key (with passphrase) or
  ssh-agent. Host keys are checked; a dead tunnel is reopened automatically.
- **Paste a connection URL** (`postgres://`, `mysql://`, `redis://`,
  `mongodb://`, `sqlite:`) to fill in the connection form; a password in
  the URL is stored only in the system keychain.
- **Test connection** in the form reports the server version and latency,
  or the error with details.
- **Import from TablePlus**, including passwords from export files.

## Browsing and editing data

- **Table view:** a spreadsheet grid with paging, sorting (click a column
  header: ascending, descending, off) and filters (⌘F). Several filters
  combine with AND; each applied filter shows as a chip in the toolbar, and
  its × removes it. Columns show their type under the name, primary keys a
  key, foreign keys "→ table"; numbers are right-aligned, enum values are
  coloured pills, and NULL / DEFAULT are pills so they never look empty.
- **Staged editing:** edits are tinted (amber edited, green new, red
  deleted) and nothing is written until you commit (⌘S). A bar below the
  grid counts them, with *Preview SQL* (exactly what will run), Discard
  (⌘⇧⌫) and Commit. Commits are one transaction where the database
  supports it.
- **Inline cell editing:** Enter or typing starts editing, Tab / ⇧Tab move
  between cells, Esc cancels. Multi-line values open a larger editor.
  Enum columns (PostgreSQL and MySQL), and columns limited by a
  `CHECK (col IN (…))` on any SQL database, open a dropdown of their values
  (1–9 pick, a letter jumps), plus NULL when the column allows it. Booleans
  get true / false (/ NULL), and Space flips one without opening it.
- **Checked as you type:** whole numbers and their range, decimals (with the
  stored form, e.g. "saved as 118.40"), `varchar(n)` length with a live
  counter, dates and times, JSON and UUIDs. ↵ / Tab won't save an invalid
  value; ↑ / ↓ step a number (⇧ by 10); UUID cells have *Generate*.
- **Date cells** show a month calendar under the cell (the time of day is
  kept), with *Today* and *Now*; time cells have *Now*. The text stays
  editable.
- **Foreign-key cells** list up to 5 rows of the referenced table matching
  the typed key or name (its first text columns); ↑ / ↓ and ↵ or a click
  pick one.
- **Arrays** (PostgreSQL) and MySQL `SET` values show as chips; binary
  columns show their type and size instead of hex.
- **⌥⌫** sets the selected cell to NULL; **⌥↵** opens the bigger editor,
  which JSON cells use by default: highlighted, *Format*, live validity,
  *Set NULL*.
- **Row detail panel** (⌘⇧I): every field of the selected row, with JSON
  pretty-printed.
- **Foreign keys:** a → in a foreign-key cell opens the referenced row.
- **Structure view:** columns, indexes, foreign keys and the table's
  `CREATE TABLE` statement.
- **New Table / Edit Columns:** add, rename, retype or drop columns, change
  NULL / default / primary key. The DDL is shown before it runs.
- **Sidebar actions:** open, copy name, new query, rename, duplicate,
  vacuum / optimize, truncate and drop.
- **PostgreSQL schemas:** tables are grouped by schema, and the schema in
  the title bar sets `search_path` so unqualified names find the chosen
  schema.
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
  One dialog picks the format, the rows (current filter, whole table, this
  page or the selected row, with counts), format options (header row,
  pretty JSON), and the destination, with a live two-row preview. Table
  exports stream every page to disk, so large tables don't fill memory; a
  long export shows progress with an estimate and Cancel. In CSV, NULL is
  an empty field and an empty string is `""`.
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
  The title bar always shows the effective mode. The confirmation names the
  environment in its colour, lists the exact SQL with what it changes
  ("1 updated · 1 deleted"), and its button says where ("Commit to prod",
  red for production or DROP / TRUNCATE). On production ↵ cancels and only
  ⌘↵ confirms. *Confirm writes* can be snoozed for 10 minutes per
  connection (in memory only). See [Safety and security](architecture/safety.md).

## Settings (⌘,)

Sections for General (session restore), Appearance (System / Light / Dark,
remembered across launches; ⌘⇧L switches too; interface size), Editor
(font size with a preview), Data grid (rows per page when a table opens),
Safe mode (what new connections start with), and Shortcuts: a searchable
list of every key binding. Changes apply at once.

## Not yet supported

ER diagrams, users & roles, schema compare, an AI assistant, more database
drivers, and multiple windows.
