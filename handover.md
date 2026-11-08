# Handover: Tabula UI redesign (in progress)

Written 2026-11-07 for the next agent; updated later the same day after
steps 5–6 and the loading states. Repo: `bhumit070/tableplus_rs`
(Tabula, a TablePlus-style database client in Rust with gpui and
gpui-component 0.9). The user asked to "redesign everything, no mistakes,
no bugs" from the Penpot design. `design.md` (untracked, written by the
user) is the spec: tokens, components, screens 01–23, cell editors, the
implementation plan, and answers to review questions under "Known gaps and
open questions → Review questions". Penpot file:
<https://design.bhumit070.bid/#/workspace?team-id=d385f656-55f3-80fe-8008-daed1669a29c&file-id=52310b26-7e62-8166-8008-e748374a2159&page-id=52310b26-7e62-8166-8008-e748374a215a>
(Penpot MCP tools need the user's browser plugin running; often not connected).

## Ground rules (from the user and org policy)

- **Never push, publish or deploy without asking.** Every push to `main` that
  touches `src/` cuts a release (v0.1.x) and updates the Homebrew tap and
  apt repo. Approval for one push doesn't cover the next.
- Commit locally after each step; end messages with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- No DB writes outside the throwaway test containers (`tabula-test-*`, see
  `compose.yaml`) and temp SQLite files. Never touch the user's other
  containers.
- No secrets in code or chat. `*.tableplusconnection` holds real passwords:
  never commit or read it.
- `design.md`, `handover.md` and the minified `packaging/icons/tabula.svg`
  were committed by the user in `faf4110`. Everything up to that commit is
  pushed (v0.1.8 was released from `4560056`); the commits listed under
  "Done since" below are local only.
- Code style: import items with `use` and refer to them by short name, no
  inline `crate::…` / `std::…` paths (CLAUDE.md, Conventions).

## How to work and verify

- Build with no warnings: `cargo build`. Format: `cargo fmt` (a pre-commit
  hook in `.githooks` does it too).
- Tests: `cargo test` (≈154). End-to-end against real servers:
  `docker compose up -d --wait`, then `TABULA_IT=1 cargo test`,
  then `docker compose down` when done. Containers may be up (`docker ps`).
- **Screenshots** (essential, the user can't check visuals for you):
  `cargo test screenshots -- --ignored --nocapture` renders the real app
  offscreen (Metal, headless) in dark and light to `target/shots/*.png`.
  Scenarios are in `src/ui/app/shots.rs` (synthetic SQLite "shop-demo"
  database). Read the PNGs and compare with the design. Screen capture of
  the real window is blocked on this Mac; use this instead.
- UI tests live in `src/ui/app.rs` `mod tests`; drive keys with
  `simulate_keystrokes`, clicks with `debug_bounds("selector")` plus
  `simulate_click`.
- Progress checklist (also kept here): scratchpad `redesign-plan.md`.

## gpui / gpui-component gotchas learned

- Key **bindings run before key-down listeners**. To override an input's
  own keys (↑/↓, Esc, Tab) inside the cell editor, add actions bound in
  `"CellEditor > Input"` (see `src/ui/mod.rs`). `capture_key_down` alone
  does not work for keys the input binds.
- macOS marks arrow keys with the `function` modifier, so don't use
  `Modifiers::modified()` to mean "no modifiers".
- Callbacks fired while a host entity is updating (e.g. from `start_edit`)
  must `window.defer(...)` before updating that host again, or gpui panics
  ("already being updated").
- `DataTable` calls `render_tr` for filler rows past the data, so check
  `row_ix < rows.len()`.
- A `Column` with `.sort(...)` gets the library's sort icon; the grid draws
  its own header (`render_th`) and sorts on header click instead.
- Rems follow the interface font size (Root sets rem = theme font size).
  Use `theme::dp(px)` for design pixels (13px base).
- `IconName` must come from `gpui_kit_assets` (full Lucide), not
  gpui-component's subset.
- Dialogs animate in; the shots harness advances the clock in `settle()`.
- `impl IntoElement` return types capture `cx`; return `AnyElement` when a
  helper's result is used alongside further `cx` borrows.
- gpui-component dialogs bind ↵ to "OK", which closes the dialog by default
  and loses a form. `ui::dialog::shell` sets `on_ok(|..| false)`; a dialog
  that wants ↵ (the history list) sets its own `on_ok` after `shell`.
- In UI tests a closing dialog needs `cx.executor().advance_clock(..)`
  before another can open. `loading::visible` uses real `Instant` time,
  while timers use the executor clock: sleep *and* advance.
- `TestPlatform::reveal_path` panics (`unimplemented!`): never reveal a
  file automatically; offer a "Show in Finder" toast action instead.
- A `Button`'s `debug_selector` isn't found by `debug_bounds`; wrap the
  button in a `div().debug_selector(..)`.
- Edits staged while a table page is loading are refused (`set_cell`);
  tests must wait for `!is_loading` and the value, not the value alone.

## Done (local commits, none pushed after `81dc901`)

| Commit | What |
| --- | --- |
| `430ea25` | App icon on all platforms (packaging/icons, build.rs, .icns/.ico) |
| `ea5ec12` | Step 1: theme from tokens (`assets/themes/tabula.py` → `tabula.json`), Geist fonts, four env tags (Testing → Development migration) |
| `6c35ef4` | Step 2: title bar (env pill, conn / db / schema breadcrumb, ⌘P go-to, safe-mode badge, New Query), rail tiles, sidebar, lifted tabs, status bar with server version; screenshot harness; 13px default UI size |
| `0c8586c` | Step 3: grid (two-line header, typed cells, pills, NULL/DEFAULT, staged-row tints), toolbar (Data/Structure/DDL, filter chips, pager, Refresh, + Row, Export), pending-changes bar; filters persist as chips |
| `5f2966d` | Step 4a: validation as you type (`ui/cell_value.rs`), number stepping, UUID generate, enum/bool dropdowns, CHECK IN lists as enums, ⌥⌫ Set NULL, ⌥↵ JSON/long-text dialog |
| `e0c09fb` | Step 4b: date calendar, foreign-key search picker (`Connection::fk_matches`), arrays as chips, binary as type + size; ↑/↓ via actions |
| `515637e` | Step 5a: commit confirmation (`ui/confirm.rs`): env banner, SQL, summary, snooze, "Commit to prod", ↵ cancels on prod |

Done since (local, not pushed):

| Commit | What |
| --- | --- |
| `517ce62` | 5b Export dialog (`copy.rs`): format tiles, row scope with counts, options, preview, destination, progress toast with Cancel; shared `ui/dialog.rs` |
| `e2e8014` | 5c Connection form: engine tiles, connection URL (`db/url.rs`), env segmented control, safe-mode notes, SSH section, inline Test connection |
| `93d6112` | User's import-convention cleanup (+ CLAUDE.md rule) |
| `df76cec` | 5d Settings: sections, shortcut list from `key_bindings()`, appearance / rows per page / default safe mode saved |
| `237a922` | 5e Home cards, Open now / Saved, env filter, `/` search, `last_used` |
| `8e7cd9d` | 6a Query toolbar (Run on, Limit via `db::target::with_limit`), result tabs with status dots, failed statement keeps earlier results (`db::BatchFailed`) and is underlined, Ln/Col, EXPLAIN tree / raw / insight |
| `03cb297` | 6b History + saved queries in one dialog (`ui/history_view.rs`) |
| `0c6dfb3` | Loading and empty states (`ui/loading.rs`): skeleton rows after 150 ms, loading chip with Stop, db switching, connecting rail tile, busy toasts, empty table / no matches |
| `e6f6f36` | Could-not-connect state on home cards |
| `e1c57e2` | 7a Command-palette look for every picker (sections, icons, key caps, highlighted matches) |
| `2c26a95` | CI: only the newest commit builds / releases (cancel older runs, draft-then-publish, head check) |
| `dabf94c` | ⌘↵ runs the statement at the cursor (`db::split::statement_at`), ⌘⇧↵ runs all; result tabs close; no tab bar for one result |
| `0df6382` | New Database… switches to the new database |
| `c095faa` | 24.6 Welcome back: launch offers last session instead of reconnecting (`always_reopen`) |
| `e5a2df8` | 24.4 Passwords step before the first keychain access (`keychain_explained`, `save_passwords`) |
| `12b44b1` | Sample database (`src/sample.rs`) |
| `3fd21d3` | 24.1–24.5 First-run onboarding (`ui/onboarding.rs`, `onboarded`) |
| `b183262` | Docs refresh |

All of the above was pushed to `main` at the user's request at the end of
that session.

Since that push (local, **not pushed**, ask first):

| Commit | What |
| --- | --- |
| `27c61b6` | 10 Filters: 17 operators per column type (`FilterKind`), ranges and lists, match all / any, WHERE preview with Copy |
| `a220e67` | 16 Process list: Live every 2 s, Active / Idle in tx / All, detail strip with "Blocked by PID", kill with confirmation |
| `02e6e1f` | 09 Row menu: Copy as / Copy all rows as submenus, Row Details, Filter by This Value, Open Referenced Row |
| `7f11294` | 06 Database switcher: Recent section and Create database |
| `6fdb5b7` | 18 New table: Blank / Timestamps / Copy of templates, type menu, live DDL |
| `089786f` | 13 CSV import mapping dialog (`ui/csv_import_view.rs`) with local validation |
| `b7f1585` | 22 Query errors suggest the column / table you meant (`db/suggest.rs`) |
| `96e9e3b` | Row panel tabs Fields / JSON / References |
| `e753b71` | 22 Connection lost banner: auto retry with back-off, Retry now, "Reconnecting…" in the status bar, edits kept |
| `7d6078f` | 08 Structure › Columns edited in place (`ui/structure_editor.rs`): staged with "was …", Preview DDL / Discard / Apply; the dialog keeps only New Table |
| `67b1a68` | Fix: ↵ in the write confirmation confirms away from production (the dialog's own ↵ binding used to just close it) |
| `6d8c714` | 08 Indexes list with Copy Definition / Drop Index… and the + Index dialog (`ui/index_editor.rs`) |
| `3b80088` | `ddl::index_entries` moved to db; end-to-end index test on each SQL server |
| `602040a` | 19 Redis key editor (`ui/redis_key_view.rs`, `db/redis_key.rs`): typed editors, TTL countdown, Rename / Delete, staged Save in one MULTI |
| `c30c8ee` | 20 MongoDB Documents (cards) and JSON views (`ui/mongo_views.rs`) |
| `8acf826` | 20 MongoDB find bar (`TableQuery.find`, JSON5 filter document) |
| `18a152a` | 05 ⌘P scope chips (Tab cycles), columns and saved queries, prefixes `>` `#` `.`, ⌥↵ copies the qualified name |
| `0937707` | Fix: `object_index` back to test-only |
| `6bba6af` | 06 ⌘K sizes and counts per database (`Connection::database_stats`), filled in after the picker opens (`Picker::set_details`) |
| `0323cda` | 19 Redis key types as coloured badges in the grid (`Kind::KeyType`, `theme::redis_type_color`) |
| `1b015de` | 08 Foreign keys: list with Copy / Drop, + Foreign key dialog (`ui/foreign_key_editor.rs`); SQLite read-only |
| `7b52ca1` | Rows referencing this: `Structure.referenced_by` (inbound keys, reversed); row menu submenu and row panel section |
| `0072b09` | 02 Sidebar approximate row counts (`approx_row_counts`) and Saved queries section (`SavedChanged` global) |
| `4322ba4` | Redis TTLs as durations in the grid (`Kind::Ttl`, `grid::duration`) |
| `53e29db` | 16 Process list: state pills, >1 min red, lock waits amber (grid `tint` hook) |
| `1dfe8a2` | 08 Column comments on Postgres / MySQL (ColumnInfo / ColDef `comment`) |
| `0ea32b3` | `db::binary`: hex written as bytes on every engine; MySQL non-UTF-8 binary read as `0x…` |
| `0275e9d` | 13 Binary cells: Save Value to File / Load File into Cell |
| `b42a2e0` | MCP server for AI tools (`src/mcp.rs`, `ui/mcp_service.rs`, Settings › MCP) |
| `f740fd1` | New logo: app icons regenerated from `packaging/icons/tabula.svg` |
| `4752188` | Onboarding redesign 24.1–24.5 from Penpot; `ui/logo.rs`; Passwords step inside onboarding with a real keychain probe and outcomes (24.4b) |

Redis / MongoDB screenshots (19-redis-hash, 19-redis-string, 20-mongo-documents,
20-mongo-json) only render with `TABULA_IT=1` and the containers up. Each
Redis test owns a database: 5 end-to-end, 6 insert-with-TTL, 7 UI test,
8 screenshots, 9 key editor.

Screenshots now also cover 05-palette, 17-open-connection, 24-welcome-back,
24-passwords and 24-1…24-5 onboarding steps, and 02-query-results, 04-edit-connection,
14-explain, 15-history, 21-settings-shortcuts, 22-loading, 22-no-matches.

Committed after this was first written (shots baseline): `src/ui/app/shots.rs` has screenshots for settings,
the new-connection form, export and the home screen with saved connections
(`21-settings`, `04-new-connection`, `12-export`, `03-home` in target/shots). They show the old styling: start step 5 from them.

Key new files: `src/ui/theme.rs` (tokens, `dp`, pills, `kbd`, `caps`,
`safe_mode_badge`, `enum_color`), `src/ui/cell_value.rs`,
`src/ui/confirm.rs`, `src/ui/app/shots.rs`, `assets/`.

## Remaining (design.md "Implementation plan")

5. ~~Dialogs~~ done (see above). Original notes:
   - **Export (12)**: `src/ui/copy.rs` `open_export`. Format tiles, rows scope,
     options, 2-row preview, destination; progress toast for long exports.
   - **Connection form (04)**: `src/ui/conn_form.rs`. Engine tiles, four-env
     segmented control, Paste connection URL (new), SSH section, safe-mode
     select with descriptions, inline Test connection.
   - **Settings (21)**: `src/ui/settings_view.rs`. Sections (General,
     Appearance, Editor, Data grid, Safe mode, Shortcuts list generated from
     `key_bindings()`).
   - **Connections home (03)**: `src/ui/home.rs`. Cards 380×128 in a 3-column
     grid, Open now / Saved groups, env filter, Import / New connection.
6. ~~Query tab~~ done except: the 2px active-statement bar in the gutter
   and the autocomplete restyle need hooks gpui-component 0.9 doesn't
   have (no gutter decorations; the completion popup is drawn by the
   library). Original notes: toolbar (Run primary, Stop, Explain, Format,
   Save, History, Run on, Limit), active-statement bar, autocomplete restyle,
   result tabs with status dots, read-only reason badge, EXPLAIN tree with
   exclusive-cost bars (design answer #21), merged History / Saved dialog.
   Status bar: add Ln / Col.
7. New features: done. Still open: the Redis sidebar key tree (19; the
   keys grid with type badges + key editor cover it); MongoDB field
   autocomplete in the find bar (20; sort is the column headers, limit the
   page size, projection the fields input).
8. States (22): done. Gap: the connection-lost state doesn't dim the
   grid (gpui's `opacity` and an absolute wash both fail to cover
   `DataTable` rows); the banner and status bar carry it instead.

Smaller leftovers: the array chip editor (cell editor #11; arrays edit as
`{a,b}` text); design.md
"Review questions": the row panel answer (#13) is applied. Headless
screenshots don't relayout after `Window::resize`; check sizes with
`simulate_resize` in UI tests instead. The MySQL binary read treats binary-typed values that are valid
UTF-8 as text (information_schema reports its text columns as binary).
`connection_switching` and `picker_opens_saved_connection` (ui tests,
both ⌘N picker) have each failed once in a loaded full
`TABULA_IT=1` run and passes alone: a timing flake in the ⌘N picker step.

Keep docs in step: README.md, `docs/` (features, shortcuts, architecture/ui,
modules, development). The docs site deploys on push.
