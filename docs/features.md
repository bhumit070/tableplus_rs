# Features

## First run

A short onboarding on a fresh install, filling the window (skippable at
every step, Esc goes back):

1. **Welcome:** what Tabula is, three things it's good at, and *Get
   started* (or *Coming from TablePlus?*, which lands on Import).
2. **Look & feel:** System, Dark or Light (← / → move; the window
   re-themes at once) and the text size, with a live preview row.
3. **Connections:** import from TablePlus (from an export file, or this
   Mac's own list), a new connection, a SQLite file, or the sample
   database. *I'll do this later* is fine too.
4. **Passwords & Keychain:** only when the choice involves a password.
   It explains what's stored where and shows the system prompt to expect;
   *Continue* triggers that prompt and says how it went (with how to
   change your mind if it was refused). *Don't save passwords* means
   Tabula asks at every connect and stores nothing.
5. **You're all set:** what was set up, four shortcuts worth knowing, and
   *Open Tabula* (the sample database opens on its `orders` table).

Installs that already have connections skip it. Nothing touches the
keychain until a password is actually involved; when that happens later
instead, the same Passwords step shows as a dialog.

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
- **Paste a connection URL** (`postgres://`, `mysql://`, `sqlserver://`,
  `redis://`, `mongodb://`, `sqlite:`) to fill in the connection form; a password in
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
  from there). If a connection drops while you work, an amber banner
  says so and retries on its own (after 5 s, then waiting longer each
  time, up to a minute), with *Retry now*; the status bar shows
  "Reconnecting…", what's on screen stays readable and staged edits are
  kept. When the server answers again, the table reloads. An empty table offers *Add row* and *Import
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
  columns show their type and size instead of hex. Right-click a binary
  cell to **Save Value to File…** or **Load File into Cell…** (up to
  16 MB, staged like any edit and written as bytes on commit).
- **⌥⌫** sets the selected cell to NULL; **⌥↵** opens the bigger editor,
  which JSON cells use by default: highlighted, *Format*, live validity,
  *Set NULL*.
- **Row detail panel** (⌘⇧I; docked beside the grid in windows 1440px
  and wider, floating over its right edge below that; drag its left edge
  to resize it between 280 and 480px, remembered), with three tabs: **Fields** (every field
  of the selected row, JSON pretty-printed, edit and copy per field),
  **JSON** (the row as one object, JSON columns nested, with Copy) and
  **References** (the row's foreign keys, each opening the referenced
  row, and *Referenced by*: the tables whose foreign keys point at this
  row, each opening their matching rows).
- **Foreign keys:** a → in a foreign-key cell opens the referenced row.
- **Structure view:** columns, indexes, foreign keys and the table's
  `CREATE TABLE` statement.
- **Editing columns in place:** Structure › Columns is editable: add,
  rename, retype or remove columns and change NULL and defaults right in
  the list (the sidebar's *Edit Columns…* opens it). Changes stage like
  data edits: amber with "was …" under what changed, green for new, red
  for removed. A bar counts them, with *Preview DDL*, *Discard* and
  *Apply*; Apply runs them as one script (transactional on Postgres,
  SQLite and SQL Server) through safe mode. Each type field has a menu of
  the engine's common types. Postgres, MySQL and SQL Server columns also
  have a **comment** (`COMMENT ON COLUMN` / part of `MODIFY COLUMN`, which
  now keeps an existing comment instead of dropping it / the
  `MS_Description` extended property). SQLite can add, rename and
  remove columns only.
- **Indexes:** Structure › Indexes lists each index with its definition;
  right-click to copy the definition or drop it (always confirmed).
  *+ Index* opens a small dialog: pick columns in order, unique or not,
  and the name follows the columns until you type your own; the
  `CREATE INDEX` is shown live.
- **Foreign keys:** Structure › Foreign Keys lists each one as
  `(column) → table (column) ON DELETE …`; right-click to copy it or drop
  it (confirmed). *+ Foreign key* picks the column, the table (from a
  menu of tables) and column it refers to, and what deleting the
  referenced row does; the statement shows live. SQLite can't change
  constraints of an existing table, so there the list is read-only.
- **New Table:** name, columns (type menu, NULL, default, key) and the
  `CREATE TABLE` shown live. It starts blank, with `id` + `created_at` /
  `updated_at`, or as a copy of another table's columns. Nothing runs
  until Create table.
- **Sidebar:** tables, views, collections, **Functions** (Postgres, MySQL
  and SQL Server functions and procedures; click one to open its `CREATE` statement
  in a query tab) and **Saved queries** (click
  one to open it in a query tab; the list updates as you save or delete
  queries), each section with its count. Tables show an approximate row
  count in mono (`12`, `1.2K`, `2.1M`) from the server's statistics, never
  a `COUNT(*)`: Postgres `reltuples`, MySQL `table_rows`, MongoDB's
  estimated document count (SQLite keeps none).
- **Sidebar actions:** open, copy name, new query, rename, duplicate,
  vacuum / optimize, truncate and drop.
- **PostgreSQL schemas:** tables are grouped by schema, and the schema in
  the title bar sets `search_path` so unqualified names find the chosen
  schema.
- **Redis:** keys grouped by namespace (`user:*`), with type (a coloured
  badge), TTL (as a duration like `59m 52s`; `—` for no expiry) and value.
  The selected key opens in a key editor beside the list: its type badge,
  size, a live TTL countdown (set an expiry or persist from its menu),
  Rename (never overwrites another key) and Delete. Each type has its own
  editor: hash field / value, list items, set members, zset member /
  score, string text (JSON shown formatted, saved back compact), stream
  entries read only. Edits stage with the grid's colours; *Preview* shows
  the commands and *Save* runs them in one `MULTI`. Big keys show their
  first 500 elements.
- **MongoDB:** three views of a collection, switched in the toolbar:
  **Documents** (the default; one card per document, nested objects and
  arrays shown formatted; click a value to edit it), **Table** (columns
  from the union of top-level fields) and **JSON** (the page as raw JSON,
  numbers and booleans kept). Edits made in any view stage together and
  save as `$set` by `_id`.
  A **find** bar above the data takes a filter document in JSON5
  (`{ status: "paid", total: { $gt: 100 } }`, dotted paths for nested
  fields); ↵ applies it on top of the filter chips, and clearing it shows
  everything again. **fields** beside it limits the documents to the
  fields listed (`name, status`; `_id` always comes along).

## Query editor

- Syntax highlighting, and **autocomplete** for tables, columns (including
  aliases), keywords and functions. Redis and MongoDB get command and
  method names.
- ⌘↵ runs the statement the cursor is in (or the selection); ⌘⇧↵ runs
  everything. **One result tab per statement** when there's more than one
  (a tab's × closes it), each with a status dot (green rows, amber rows affected, red
  error) and rows · time. When a statement fails, the ones before it keep
  their results, the error gets its own tab, and the statement is
  underlined in the editor. When the error names a column or table that
  doesn't exist, Tabula suggests the closest one ("Did you mean total?")
  and Use fixes it in the editor.
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
  This Value** (or `is null`), Open Referenced Row on a foreign key, **Rows
  Referencing This ›** (one entry per table that points here), Import
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
  filtered to Active / Idle in transaction / All. States show as pills,
  statements running over a minute in red, lock waits in amber. The selected session's
  full query, client and start time show below the list, with "Blocked by
  PID n" to jump to the session holding it up (Postgres). Cancel query and
  Kill session always show the exact statement and ask first; your own
  session isn't listed.
- **Backup / Restore** with the database's own tools (`pg_dump` / `psql`,
  `mysqldump` / `mysql`); SQLite is copied with `VACUUM INTO`. A
  Postgres dump made by a newer `pg_dump` restores into an older server. SQL Server
  writes its backups on the server (`BACKUP DATABASE`), so Tabula doesn't
  offer one.
- **Create / drop databases** (a new database opens right away) and the
  **database switcher** (⌘K): Recent databases first (the current one
  marked), then all of them, then Create database…. Each row fills in its
  size and count as they arrive: sizes on Postgres, SQL Server and MongoDB, sizes and
  table counts on MySQL, key counts on Redis ("—" where the server
  doesn't say).

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
Safe mode (what new connections start with), MCP, and Shortcuts: a
searchable list of every key binding. Changes apply at once.

## AI tools (MCP)

Turn on **Settings › MCP** and AI tools such as Claude Code or Cursor can
work with your saved connections through an MCP server on this computer:
list databases and tables, read structure and rows, run queries, change
rows, see and stop processes. Copy the ready-made `claude mcp add …`
command or JSON config from Settings. It listens on `127.0.0.1` only, needs
a token, never sends passwords, and every write follows the connection's
safe mode (confirm modes ask in the Tabula window first). See
[MCP server](mcp.md).

## SQL Server

- Connects with SQL Server logins (user and password) to SQL Server 2012+
  and Azure SQL; Azure's redirect to another node is followed. Unchecked
  *Require SSL* still encrypts when the server can; the certificate is
  checked only with *Verify server certificate*.
- The editor speaks T-SQL: `GO` on its own line ends a batch, as in SSMS.
  A batch is split on `;` so each statement gets its own result, except
  when that would change its meaning (`DECLARE`, `BEGIN … END`, `IF`,
  `CREATE PROCEDURE …`), which runs as one batch. A procedure returning
  several result sets gets a tab for each. ⌘. cancels with the protocol's
  own attention signal, so no KILL permission is needed.
- The row **Limit** adds `TOP (n)`; *New Query for This* writes
  `SELECT TOP 100`. Format keeps every `GO` on its own line.
- Values show as SSMS does: `bit` as 1 / 0 (a checkbox in the grid),
  `datetime` to the millisecond, GUIDs upper case, binary as `0x…`. Text
  you write goes in as `N'…'`, so any character survives.
- Read-only safe mode is enforced by Tabula: SQL Server has no read-only
  session. For a hard guarantee, connect with a login that can only read
  (`db_datareader`).
- Not yet: EXPLAIN (SHOWPLAN XML), backup / restore, Windows / Entra ID
  authentication and named instances (`host\instance`): give the port.

## Not yet supported

ER diagrams, users & roles, schema compare, an AI assistant, more database
drivers, and multiple windows.
