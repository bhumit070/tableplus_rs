# Features

## First run

A short onboarding on a fresh install (skippable at every step): Welcome,
Look & feel (theme with live previews, text size), Connections (import from
TablePlus, new connection, open a SQLite file, or the sample database) and
You're set, with the shortcuts worth knowing. Installs that already have
connections skip it. Nothing touches the keychain until a password is
actually involved.

## Connections

- A **connections home** of cards, grouped *Open now* and *Saved*: engine,
  endpoint, SSH / TLS / safe-mode tags, environment and when it was last
  used. Search with `/`, filter by environment. Clicking an open
  connection switches to it; a saved one opens with a double click.
- **Sample database:** "Try the sample database" (on an empty Connections
  screen, and in onboarding) opens a small SQLite shop of customers,
  products and orders, about 2,000 rows, written to the config folder on
  first use. No password, no network; delete it like any connection.
- **Environment tags** (local, development, staging, production), shown as
  colored badges. Connections saved with the old
  *testing* tag open as development.
- **No connection limit.** Each open connection gets an icon in the left
  rail; ⌘1…⌘9 jump between them.
- **Session restore:** at launch, a "Pick up where you left off?" card
  offers last time's connections and tabs (including unsaved query text);
  Reopen (↵) connects them. Nothing connects, and the keychain isn't
  touched, until you ask. "Don't ask, always reopen" brings back the
  automatic reconnect; both are in Settings.
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
  header: ascending, descending, off) and filters (⌘F). Each condition
  offers the operators that fit its column's type: `=`, `≠`, `in`,
  `not in`, `is null`, `is not null` for any column; `<`, `≤`, `>`, `≥`,
  `between` for numbers and dates; `contains`, `starts with`, `ends with`
  (case-insensitive), `LIKE` and `~ regex` (Postgres, MySQL) for text.
  Conditions combine with *all* or *any*, and the editor shows the `WHERE`
  clause they make (copyable). ↵ applies, Esc closes. Each applied filter
  shows as a chip in the toolbar, and its × removes it. Columns show their type under the name, primary keys a
  key, foreign keys "→ table"; numbers are right-aligned, enum values are
  coloured pills, and NULL / DEFAULT are pills so they never look empty.
- **Loading and empty states:** anything that takes longer than a blink
  shows placeholder rows (never a spinner over a blank panel) and a
  "Loading orders · 1.2 s · Stop" chip; Stop really stops (for a query, on
  the server). Switching database shows "Switching to …" in the title bar
  and placeholder rows in the sidebar; a connection being opened gets a
  spinner tile in the left rail; backups and restores keep a progress
  toast until they finish. A connection that couldn't be opened keeps the
  reason on its home card, with Retry and Details (and Edit connection…
  from there). An empty table offers *Add row* and *Import
  CSV…*; a filter that matches nothing says so and offers *Clear filters*.
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
  NULL and defaults, with the DDL shown live as you edit; each type field
  has a menu of the engine's common types. A new table starts blank, with
  `id` + `created_at` / `updated_at`, or as a copy of another table's
  columns. Nothing runs until Create table / Apply.
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
- ⌘↵ runs the statement the cursor is in (or the selection); ⌘⇧↵ runs
  everything. **One result tab per statement** when there's more than one
  (a tab's × closes it), each with a status dot (green rows, amber rows affected, red
  error) and rows · time. When a statement fails, the ones before it keep
  their results, the error gets its own tab, and the statement is
  underlined in the editor.
- **Run on** shows (and switches, ⌘K) the database; **Limit** adds
  `LIMIT n` to SELECTs that have none (history keeps what you typed).
- A **read-only** badge says why a result can't be edited. The status bar
  shows the cursor's line and column.
- **Stop** (⌘.) cancels the running statement on the server
  (`pg_cancel_backend` / `KILL QUERY`), not just in the app.
- **Editable results:** a single-table `SELECT` can be edited like a table.
- **SQL formatter** (⌘⇧F). **Query history** (⌘Y) and **saved queries**
  (⌘⇧S to save, ⌘⇧O to browse; for one connection or all of them) share one
  dialog: search, an all / ok / failed filter, and a preview with Open in
  new tab, Insert at cursor and Save as… (Delete for saved ones).
- **EXPLAIN** (⌘E) as a plan tree (or the raw output). Each step's bar is
  its *own* cost or time as a share of the plan (red for the biggest); row
  estimates that are off by more than 10× are amber. One plain-language tip
  for the most expensive step, such as an index for a filtered full scan
  (with Copy CREATE INDEX). The *Analyze* switch shows actual times; it runs
  inside a transaction that is rolled back. Copy plan copies the raw output.

## Copy, export and import

- **Row menu** (right-click): what's selected ("orders #10356"), Copy
  Cell, **Copy as ›** JSON / INSERT / UPDATE / CSV / TSV / Markdown, the
  column's values as `IN (…)` or the column names, **Copy all rows as ›**,
  Export…, Row Details, Edit Cell, Set NULL, Duplicate Row, **Filter by
  This Value** (or `is null`), Open Referenced Row on a foreign key, Import
  CSV…, and Delete Row (staged).
- **Export** (⌘⇧E) to CSV, TSV, JSON, JSON Lines, Markdown, HTML or SQL.
  One dialog picks the format, the rows (current filter, whole table, this
  page or the selected row, with counts), format options (header row,
  pretty JSON), and the destination, with a live two-row preview. Table
  exports stream every page to disk, so large tables don't fill memory; a
  long export shows progress with an estimate and Cancel. In CSV, NULL is
  an empty field and an empty string is `""`.
- **Import CSV** into an existing table: a dialog shows the file (rows,
  columns, size, encoding, delimiter), options (header row, empty fields as
  NULL, skip rows that fail validation) and the column mapping — each CSV
  column to a table column or skip, with sample values and ✓ / ⚠ / ×.
  Values are checked locally with the cell editors' rules, with line
  numbers for problems; unmapped required columns block Import. All rows
  go in one transaction, through safe mode.
- **Process list** (⌘⇧P): server sessions, live every 2 s (or paused),
  filtered to Active / Idle in transaction / All. The selected session's
  full query, client and start time show below the list, with "Blocked by
  PID n" to jump to the session holding it up (Postgres). Cancel query and
  Kill session always show the exact statement and ask first; your own
  session isn't listed.
- **Backup / Restore** with the database's own tools (`pg_dump` / `psql`,
  `mysqldump` / `mysql`); SQLite is copied with `VACUUM INTO`.
- **Create / drop databases** (a new database opens right away) and the
  **database switcher** (⌘K): Recent databases first (the current one
  marked), then all of them, then Create database….

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
