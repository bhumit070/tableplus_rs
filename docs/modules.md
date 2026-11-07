# Source files

Every file in `src/`, what it's for and roughly how big it is. Each file
also starts with a `//!` comment saying the same in more detail.

## Top level

| File | Lines | Purpose |
| --- | ---: | --- |
| `main.rs` | ~130 | Entry point: starts gpui, loads settings, binds keys, registers actions, builds the menu bar and opens the window. On Windows release builds, `windows_subsystem = "windows"` prevents a console window. |
| `runtime.rs` | ~60 | The tokio runtime the drivers run on, and `spawn` / `spawn_detached` to reach it from the UI. Panics become errors. |
| `store.rs` | ~230 | Config folder, saved connections, keychain access, `read_json` (moves damaged files aside), `write_atomic`. |
| `settings.rs` | ~170 | User settings (font sizes, session restore, appearance, rows per page, default safe mode) as a gpui global, saved to `settings.json`. |
| `restore.rs` | ~80 | What `session.json` holds, for reopening last time's connections and tabs. |
| `sample.rs` | ~170 | The sample database: a SQLite shop generated in SQL on first use. |
| `saved.rs` | ~100 | Saved queries in `saved_queries.json`, per connection or global. |
| `history.rs` | ~190 | Query history in `history.db` (SQLite through sqlx), pruned to 5,000 entries per connection, with a schema version. |
| `export.rs` | ~450 | Grid data → text: Copy as JSON / INSERT / UPDATE / CSV / Markdown / `IN (…)`, and streaming export (`Encoder`: header, rows, footer) to CSV, TSV, JSON, JSON Lines, Markdown, HTML or SQL. |
| `csv_import.rs` | ~230 | CSV parsing (comma, semicolon or tab; UTF-8 or UTF-16; quote-aware so NULL and `""` stay distinct), column matching and the INSERTs for Import CSV. |
| `import.rs` | ~400 | TablePlus import: `.tableplusconnection` exports (JSON, usually encrypted) and TablePlus's own `Connections.plist` on macOS. |
| `rncryptor.rs` | ~90 | RNCryptor v3 decryption for password-protected TablePlus exports. |

## Database layer: `src/db/`

| File | Lines | Purpose |
| --- | ---: | --- |
| `mod.rs` | ~700 | `Connection`, `ConnectionConfig`, `Driver`, `EnvTag`, `SafeMode` and the shared data types (`QueryResult`, `TableQuery`, `Change`, `Structure`, …). Adds timeouts and readable errors to driver calls. |
| `sql.rs` | ~1,270 | PostgreSQL, MySQL and SQLite through sqlx: connect / probe, listing, structure, paging, editor batches, commits, processes, admin statements. |
| `redis_kv.rs` | ~630 | Redis: key listing with SCAN, the *keys* table, one-command-per-line editor, pipelined commits. |
| `mongo.rs` | ~650 | MongoDB: collections as tables, mongosh-style call parsing (JSON5), filters, commits. |
| `ssh.rs` | ~650 | SSH tunnel: login (password, key, agent), host-key checks, local forwarding, reopening dropped sessions. Includes a test SSH server. |
| `cancel.rs` | ~110 | `Cancel`: stops a running batch, cancelling server-side first. |
| `safety.rs` | ~310 | Safe-mode decisions: does this text only read? |
| `split.rs` | ~160 | Splits SQL into statements (quotes, comments, dollar quotes). |
| `format.rs` | ~500 | SQL formatter (⌘⇧F). |
| `explain.rs` | ~320 | EXPLAIN output → `PlanNode` tree for each database. |
| `ddl.rs` | ~260 | CREATE / ALTER TABLE for the table editor. |
| `target.rs` | ~250 | Finds the table a SELECT reads from (editable results). |
| `url.rs` | ~280 | Reads a pasted connection URL into form fields; the password comes back separately, so it only reaches the keychain. |
| `backup.rs` | ~310 | Backup / restore with `pg_dump`, `psql`, `mysqldump`, `mysql`; SQLite `VACUUM INTO`. |
| `it.rs` | ~1,220 | Tests only: end-to-end checks against real servers (`TABULA_IT=1`). |

## User interface: `src/ui/`

| File | Lines | Purpose |
| --- | ---: | --- |
| `mod.rs` | ~430 | Actions, key bindings, `Shared` (one open connection), `guarded` (safe-mode prompts), `prompt_text`, colors. |
| `app.rs` | ~1,800 | `AppView`: window root, connection rail, connecting (incl. SSH host-key prompts), session save / restore, global shortcut routing, theme and zoom. Also the UI tests. |
| `history_view.rs` | ~640 | Query history and saved queries in one dialog: tabs, search, status filter, preview, open / insert / save / delete. |
| `loading.rs` | ~220 | Loading states: delayed skeleton rows, the loading chip with elapsed time and Stop, busy toasts. |
| `home.rs` | ~800 | Connections home: cards grouped Open now / Saved, search (`/`), environment filter, import, new / edit / duplicate / delete. |
| `conn_form.rs` | ~960 | New / edit connection form: engine tiles, connection URL, environment, safe mode, SSH section, inline Test connection. |
| `session.rs` | ~1,890 | One open connection: sidebar tree, database / schema switching, tabs, history / saved-query pickers, backup / restore, create / drop database, sidebar menus. |
| `table_tab.rs` | ~1,500 | Table tab: filters, grid, pager, Structure view, commit, CSV import, foreign-key navigation. |
| `query_tab.rs` | ~1,170 | Query tab: editor, run / stop, result tabs, editable results, EXPLAIN view. |
| `grid.rs` | ~480 | `Grid`: the table delegate holding rows and staged edits. |
| `cell_edit.rs` | ~270 | In-place cell editing shared by table and query tabs. |
| `completion.rs` | ~590 | Autocomplete for SQL, MongoDB and Redis. |
| `copy.rs` | ~970 | "Copy as …" menu items and the Export dialog: format tiles, row scope, options, preview, destination; streams to a temp file, then renames it, with a progress toast and Cancel. |
| `dialog.rs` | ~180 | The shared dialog look (surface, title, footer) and its controls: section labels, tiles, radio rows. |
| `row_panel.rs` | ~160 | Row detail panel (⌘⇧I). |
| `table_editor.rs` | ~430 | New Table / Edit Columns dialog. |
| `processes.rs` | ~230 | Process list (⌘⇧P). |
| `passwords.rs` | ~210 | The "Passwords" step shown before Tabula first uses the keychain; "Don't save passwords" turns it off. |
| `picker.rs` | ~250 | Searchable pick-one dialog. |
| `settings_view.rs` | ~640 | Settings dialog: sections, and the shortcut list built from `key_bindings()` with names from the menu bar. |
| `theme.rs` | ~70 | Bundled Geist fonts and the Tabula Dark / Light themes. |

## Other files

| Path | Purpose |
| --- | --- |
| `Cargo.toml`, `Cargo.lock` | Dependencies (see [Dependencies](dependencies.md)) and `.deb` packaging metadata. |
| `rust-toolchain.toml` | Pins the Rust compiler version. |
| `compose.yaml` | Throwaway database servers for the end-to-end tests (see [Building and testing](development.md#tests)). |
| `assets/fonts/` | Geist and Geist Mono, embedded in the binary (SIL OFL, `OFL.txt`). |
| `assets/themes/` | `tabula.py` turns the design tokens into `tabula.json`, the Tabula Dark / Light themes. |
| `build.rs` | On Windows, embeds `packaging/windows/tabula.ico` as the app icon. |
| `packaging/icons/` | `tabula.svg`, the app icon, and `render.sh`, which renders the `.icns`, `.ico` and PNG from it. |
| `packaging/windows/tabula.wxs` | WiX installer definition for the `.msi`. |
| `packaging/linux/tabula.desktop` | Linux desktop entry installed by the `.deb`. |
| `.github/workflows/release.yml` | Tests, builds and publishes releases. |
| `.github/workflows/docs.yml` | Builds and publishes this site. |
| `mkdocs.yml`, `docs/` | This documentation. |
