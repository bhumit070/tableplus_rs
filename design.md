# Tabula UI redesign

The written companion to the Penpot file. Penpot has the pixels; this file has
the rules, the reasoning, and how each piece maps to the code.

**Penpot file:**
<https://design.bhumit070.bid/#/workspace?team-id=d385f656-55f3-80fe-8008-daed1669a29c&file-id=52310b26-7e62-8166-8008-e748374a2159&page-id=52310b26-7e62-8166-8008-e748374a215a>

Status: **proposal, not implemented.** Nothing in `src/` has changed yet.

## Contents

- [How to read the Penpot file](#how-to-read-the-penpot-file)
- [Principles](#principles)
- [Foundations](#foundations): colour, environment tags, type, spacing, icons
- [Components](#components)
- [Window layout](#window-layout)
- [Screens](#screens): 01–23, one section each
- [Cell editors](#cell-editors): one editor per column type
- [Keyboard](#keyboard)
- [Existing vs new](#existing-vs-new)
- [Implementation plan](#implementation-plan)
- [Known gaps and open questions](#known-gaps-and-open-questions)

## How to read the Penpot file

- **★ Start here** (top of the canvas) indexes every board.
- **Left column:** `00 · Foundations`, `00b · Icons (Lucide)`, `07 · Cell editors`.
- **Main column:** screens `01`–`23` top to bottom, each 1512×944 (MacBook 14"
  default window).
- **Pink numbered markers** on a screen live in the `◎ Annotations` layer. Hide
  that layer for a clean view.
- **Cream "Developer spec" boards** to the right of each screen explain every
  marker: sizes, states, keyboard, and the source file it touches.
- **Library:** colours `Tabula Dark/*` and `Tabula Light/*`, typographies
  `Tabula/*`, components `Icons/<name>`. Design tokens: sets `tabula-dark` and
  `tabula-light`, switched by the themes `Mode / Dark` and `Mode / Light`.
- **Comment threads** on the key rules show in Penpot's Comments panel.

If an exported PNG ever disagrees with the editor, trust the editor: Penpot's
exporter caches renders per shape.

## Principles

1. **Data first.** Chrome is quiet greys. Colour is reserved for data meaning
   (types, keys, enums) and state (edited, new, deleted, production).
2. **Nothing writes by surprise.** Edits stage until Commit. Production always
   shows its colour and the exact SQL before anything runs.
3. **Keyboard everywhere.** Every action shows its shortcut next to it, and
   existing bindings are kept.
4. **One component, many places.** The same grid, cell editors, pills, dialogs
   and staged-change bar work for tables, query results, Redis and MongoDB.
5. **Say what happened.** Empty and error states explain in one sentence and
   offer the one action that fixes it.

## Foundations

### Colour tokens

Use the token name in code, never the raw hex. Light values are what the
light theme uses for the same token.

| Token | Dark | Light | Used for |
| --- | --- | --- | --- |
| `bg/canvas` | `#0B0C0F` | `#FFFFFF` | Window and grid background |
| `bg/rail` | `#0F1013` | `#F4F4F6` | Title bar, connection rail, tab bar, status bar |
| `bg/sidebar` | `#121317` | `#F8F8FA` | Sidebar, side panels, grid header |
| `bg/surface` | `#16171C` | `#FFFFFF` | Inputs, segmented controls, secondary buttons |
| `bg/raised` | `#1C1E24` | `#FFFFFF` | Popovers, menus, floating bars |
| `bg/hover` | `#22242B` | `#ECEDF1` | Hover, selected segment |
| `bg/zebra` | `#0D0E12` | `#FAFAFB` | Odd grid rows |
| `border/default` | `#24262D` | `#E2E3E8` | Panel dividers, input borders |
| `border/subtle` | `#1D1F25` | `#EDEEF1` | Grid lines |
| `text/primary` | `#E8E9ED` | `#17181C` | Headings, values |
| `text/secondary` | `#A4A8B3` | `#4B4F58` | Labels, inactive items |
| `text/muted` | `#6B707C` | `#7E838D` | Hints, column types, placeholders |
| `text/faint` | `#4A4E58` | `#A7ABB3` | Counts, disabled, shortcut hints |
| `accent/primary` | `#7C8CFF` | `#4F5FE8` | Primary buttons, selection, focus ring |
| `semantic/success` | `#3FB97A` | `#1F9D5C` | New rows, connected |
| `semantic/warning` | `#E5A93B` | `#B7791F` | Edited cells, safe mode "confirm" |
| `semantic/danger` | `#EF5A5A` | `#D93C3C` | Deleted rows, destructive actions |
| `semantic/info` | `#4FA3F7` | `#2B7BD4` | Foreign keys, JSON keys |
| `syntax/keyword` | `#B07CFF` | `#8A4FE0` | SQL keywords, bool/null, views, SQL functions as values |
| `syntax/number` | `#3CC7C2` | `#1E9E99` | Numbers in filters, query-tab glyph |

Syntax highlighting (editor, JSON, SQL previews): keyword `syntax/keyword`,
string `semantic/success`, number `semantic/warning`, function call
`semantic/info`, comment `text/muted`.

Light-theme semantic colours are about 20% darker so text and 1px borders keep
≥ 4.5:1 / 3:1 contrast on white. Tinted fills keep the same opacities in both
themes.

**State tints** (fill opacity on the base colour):

| State | Fill | Marker |
| --- | --- | --- |
| Selected row | accent 12% | 2px accent bar on the left edge |
| Edited cell | warning 14% | 1px warning border |
| New row (staged insert) | success 10% | 2px success bar |
| Deleted row (staged) | danger 10% | 2px danger bar, text muted + strike-through |
| Highlighted menu item | accent 16% | none |
| Pills (env, enum) | colour 14–16% | text in the full colour |

### Environment tags

**Decision: four tags.** The app has five today (`EnvTag` in
`src/db/mod.rs`: Local, Development, Testing, Staging, Production). The
redesign drops Testing and recolours the rest, as in the Penpot screens:

| Tag | Today (`EnvTag::color`) | New token | Pill text |
| --- | --- | --- | --- |
| Local | `#8E8E93` grey | `semantic/success` (green) | `LOCAL` |
| Development | `#34C759` green | `semantic/info` (blue) | `DEV` |
| Staging | `#FF9F0A` orange | `semantic/warning` (amber) | `STAGING` |
| Production | `#FF453A` red | `semantic/danger` (red) | `PROD` |
| ~~Testing~~ | `#5AC8FA` cyan | removed | — |

**Migration:** saved connections tagged Testing load as **Development**, the
closest meaning. `EnvTag` serializes as the variant name (no `rename_all`), so
`connections.json` holds `"Testing"`; put `#[serde(alias = "Testing")]` on
`Development` and the next save writes `"Development"`. The importer
(`src/import.rs:98`) matches TablePlus's `Enviroment` string against
`EnvTag::label()`, so once Testing is gone `"testing"` would silently fall back
to Local. Map it to Development explicitly there. Nothing else is stored per
tag, so no data is lost.

`EnvTag::color` moves onto the tokens, which keeps env colours consistent with
the rest of the UI and gives them light-theme variants. The environment colour
drives the title-bar pill, the rail tile's env dot, connection cards, the
commit-confirmation banner, and the Automatic safe mode (Production ⇒ confirm
writes).

### Typography

Geist for UI, Geist Mono for data, identifiers and code.

| Style | Font | Size / weight | Line height | Use |
| --- | --- | --- | --- | --- |
| Display | Geist | 22 / 600 | 1.2 | Home and board headings |
| Title | Geist | 16 / 600 | 1.25 | Panel titles, dialog headers (dialogs use 17) |
| Body | Geist | 13 / 400 | 1.4 | Sidebar items, menus, body text |
| UI / Medium | Geist | 12 / 500 | 1.3 | Buttons, tabs, chips, labels |
| Label caps | Geist | 11 / 600, +0.8 tracking, UPPERCASE | 1.2 | Section headers (TABLES, RECENT) |
| Mono / Cell | Geist Mono | 12 / 400 | 1.3 | Grid cells, values in inputs |
| Mono / Small | Geist Mono | 10 / 400 | 1.2 | Column types, row counts |
| Code / Editor | Geist Mono | 13 / 400 | 1.6 | SQL editor (size is a user setting) |

The existing interface and editor font-size settings scale these styles; the
ratios stay the same.

**Shipping the fonts:** nothing bundles fonts today (gpui-kit-assets only
embeds SVG icons, and the app never sets a font family). Add the Geist and
Geist Mono `.ttf` files (SIL OFL) under `assets/fonts/` and register them in
`main.rs` after `gpui_component::init` with
`cx.text_system().add_fonts(vec![Cow::Borrowed(include_bytes!(…)), …])`.
Confirm that API in gpui-pre 0.3.11; it exists in gpui 0.2.2. Then set
`font_family = "Geist"` and `mono_font_family = "Geist Mono"` in the theme
config (see [Implementation plan](#implementation-plan) step 1), not only on
the live `Theme`: switching mode reloads fonts from the config.

### Spacing and radius

- **4px grid.** Common steps are 4, 8, 12, 16, 20 and 24.
- **Radius:** 4 for cells and small pills, 6 for fields, 7 for controls and
  buttons, 10 for rail tiles and menus, 12 for cards and floating bars, 14 for
  dialogs.
- **Heights:** buttons 28 (32 in dialog footers), inputs 30–32, grid rows 32,
  grid header 40, sidebar rows 28, menu items 28–30.

### Icons

[Lucide](https://lucide.dev), 16px, 1.5px stroke (1.3px at 12–14px). The app
already uses `gpui_kit_assets::IconName` (e.g. `src/ui/home.rs:13`), which is
the full Lucide catalogue (about 1,800 icons; variant = PascalCase of the file
name). gpui-component's own `IconName` is only a ~100-icon subset, so don't use
that one. Default colour `text/secondary`; active or selected `accent/primary`.
Board `00b` lists the 52 icons used. All exist except three, which use these
names instead:

| Penpot name | Use in code |
| --- | --- |
| `filter` | `IconName::Funnel` (or `ListFilter`) |
| `history` | `IconName::RotateCcwClock` (or `RotateCcw`) |
| `trash-2` | `IconName::Trash` |

A truly custom icon needs `Icon::empty().path("icons/<name>.svg")` plus an
`AssetSource` that checks the app's own embedded folder before falling back to
`AllAssets`.

## Components

| Component | Spec |
| --- | --- |
| **Primary button** | 28px, radius 7, `accent/primary` fill, text `bg/canvas` 12/600. One per view at most. |
| **Secondary button** | `bg/surface` fill, `border/default` 1px, text `text/secondary` 12/500. |
| **Ghost button** | No fill or border; text `text/secondary`. Low-priority actions (Save, History). |
| **Danger button** | Secondary style with `semantic/danger` text, or a danger fill for production confirmations. |
| **Shortcut hint** | Inside or after a button, `text/faint` 11px (e.g. `Run ⌘↵`). |
| **Input** | 30–32px, radius 6–7, `bg/canvas` fill, `border/default`; focus = 1.5px accent inner border. Values in mono. |
| **Segmented control** | `bg/surface` with 1px border, 3px inset; active segment `bg/hover`, text `text/primary` 500. |
| **Pill** | 18–20px, radius 5 (tags) or 9 (enum values); tinted fill + full-colour text; enum pills have a 6px dot. |
| **NULL pill** | `bg/hover`, mono 10.5/500 `text/muted` "NULL". Never an empty-looking cell. |
| **DEFAULT pill** | `syntax/keyword` 14%, mono "DEFAULT". |
| **Kbd** | 18–20px, `bg/surface` or `bg/raised`, 1px border, 11px `text/secondary`. |
| **Menu / popover** | `bg/raised`, radius 10, 1px `#2E3038` border, shadow 0/16/40 black 55%; items 28–30px inset 5–6px; separators 1px `border/default`. |
| **Dialog** | `bg/sidebar`, radius 14, over a 55% black backdrop; title 17/600 at 24px inset; footer separated by a 1px line, actions right-aligned. |
| **Floating bar** | `bg/raised`, radius 12, shadow 0/8/24; centred 18px above the bottom of the grid; only while something is pending. |
| **Toast** | Bottom-right, 340px, `bg/raised`, radius 12; progress bar 6px. |
| **Connection tile** | 36×36, radius 10, env-tinted background, two-letter initials, 10px env dot bottom-right with a 2px ring; active = 2px accent outer ring + 3×20 accent bar on the rail edge. |

## Window layout

```
┌──────────────────────────── title bar 44 ──────────────────────────────┐
│ ● ● ●  [PROD] conn / db ⌄ / schema ⌄   [ Go to… ⌘P ]   safe mode  + New Query  ◐ │
├──────┬───────────┬───────────── tab bar 40 ─────────────┬──────────────┤
│ rail │ sidebar   │ toolbar 48                           │ row panel    │
│  60  │   260     │──────────────────────────────────────│   340        │
│      │ (200–400) │ grid / editor + results              │ (optional)   │
├──────┴───────────┴──────────────────────────────────────┴──────────────┤
│ status bar 26                                                           │
└─────────────────────────────────────────────────────────────────────────┘
```

## Screens

Each section lists what the screen is for, its key rules, and the files it
touches. The Penpot spec sheet beside each screen has the full marker list.

### 01 · Workspace: table data

The main window while browsing a table.

- **Title bar:** env pill + connection name + `database ⌄` + `schema ⌄`
  (Postgres). Database opens the switcher (06); schema sets `search_path`. A
  centred go-to bar (⌘P, screen 05). Right side: a safe-mode badge that's
  always visible while connected (Off muted, Confirm amber, Read-only red),
  then New Query and theme toggle.
- **Rail:** one tile per open connection (⌘1–⌘9), dashed `+` tile (⌘N),
  settings pinned to the bottom. Tooltip on hover: full name + engine.
- **Sidebar:** filter field, collapsible sections (Tables, Views, Functions,
  Saved queries) with counts. 28px rows; approximate row counts in mono
  (`12`, `1.2K`, `2.1M`). Selected row: accent 14% + accent icon.
- **Tabs:** the active tab is "lifted" into the toolbar (canvas background,
  border, no bottom edge). An unsaved query shows a dot instead of ×.
- **Toolbar:** `Data | Structure | DDL` switch; filter **chips** (column,
  operator in mono, value coloured by type); `+ Filter ⌘F`; pager, Refresh,
  `+ Row`, Export.
- **Grid:** two-line header (name 12/600, type in mono 10px; PK key glyph; FK
  `→ table` in blue; sort arrow in accent). Staged-edit states as in
  [Colour tokens](#colour-tokens). Numbers right-aligned.
- **Pending-changes bar:** `3 pending changes · 1 edited · 1 new · 1 deleted`
  with Preview SQL, Discard (⌘⇧⌫), Commit (⌘S).
- **Row panel (⌘⇧I):** form view of the selected row with the same cell
  editors; JSON pretty-printed with syntax colours; tabs Fields / JSON /
  References.
- **Status bar:** connection dot + server version + tunnel + latency; right
  side shows the active tab's context.

Files: `app.rs`, `session.rs`, `table_tab.rs`, `grid.rs`, `cell_edit.rs`,
`row_panel.rs`, `mod.rs`.

### 02 · Workspace: query editor

- **Toolbar:** Run (the only primary button; ⌘↵), Stop (enabled only while
  running; ⌘.), Explain ⌘E, Format ⌘⇧F, then ghost Save ⌘⇧S and History ⌘Y.
  Right side: **Run on** database and **row limit** (new).
- **Active-statement bar:** a 2px accent bar in the gutter marks the
  statement ⌘↵ will run.
- **Autocomplete:** 380px popup, max 5 rows; kind badge (`c` column blue, `ƒ`
  function violet, table accent, keyword muted); matched prefix bold accent;
  footer with the item's full name, type and constraints.
- **Result tabs:** one per statement with a status dot (green rows, amber rows
  affected, red error) and `rows · time`. A failing statement opens its tab
  with the error and marks the line.
- **Read-only badge** explains why a result isn't editable ("aggregate",
  "join", "no primary key").

Files: `query_tab.rs`, `completion.rs`, `grid.rs`.

### 03 · Connections home (⌘0)

- Search (`/` focuses it), environment filter, Import from TablePlus…, New
  connection.
- Cards 380×128 in a 3-column grid, grouped **Open now** / **Saved**. Engine
  badge (PG `#4FA3F7`, MY `#F29D4B`, SQ `#3CC7C2`, RD `#EF5A5A`, MG
  `#3FB97A`), name, endpoint in mono, up to 3 tags (SSH, TLS, safe mode), env
  pill, last used or "Open".
- Clicking an open connection switches to its tile instead of opening it again.
- Empty state: a centred New connection / Import, plus where passwords live.

Files: `home.rs`, `import.rs`, `store.rs`.

### 04 · New / edit connection

- 680×760 dialog. Engine tiles decide the fields. **Paste connection URL**
  (new) parses `postgres://`, `mysql://`, `redis://`, `mongodb://`, `sqlite:`;
  a password in the URL goes to the keychain only.
- Environment as a segmented control (Local / Dev / Staging / Prod). Password field shows
  where the secret lives (Keychain / Credential Manager / Secret Service).
- SSH section collapses to its header when off; auth = Password / Key file /
  Agent. Host-key behaviour unchanged.
- Safe-mode select with one-line descriptions; Automatic explains itself
  ("confirm writes because environment is Production").
- **Test connection** reports inline (spinner → green version + latency, or
  red error with Details). Save & connect (⌘↵).

Files: `conn_form.rs`, `store.rs`, `db/`.

### 05 · Command palette (⌘P), new

Go-to-anything for the current connection: tables, columns, saved queries,
commands. 640px, 96px from the top. Scope chips (Tab cycles), fuzzy matches in
bold accent, footer legend (`⌘↵` new tab, `⌥↵` copy qualified name). Prefixes
`>` commands, `#` saved queries, `.` columns.

Files: `picker.rs`. ⌘P is unused today; **⌘K stays the database switcher.**

### 06 · Database switcher (⌘K)

Popover under the database name. Focused search; **Recent** (up to 3, current
marked) then **All databases**; table count and size per row, loaded lazily
(`—` while loading). Create database… at the bottom; Drop lives in the row's
right-click menu and needs the name typed. Redis lists db 0–15 with key
counts; SQLite hides the switcher.

Files: `session.rs` (`SwitchDatabase`).

### 07 · Cell editors

See [Cell editors](#cell-editors).

### 08 · Table structure

Edit the structure in place instead of in a separate dialog: name, type
(combobox with enums and domains first), NULL, default (functions blue,
literals green), comment. Schema changes stage with the same colours as data
edits (altered cell shows `was …`). Indexes and FKs are listed below with
right-click actions; `+ Index` opens a small dialog. A Preview DDL / Discard /
Apply bar; Apply runs in one transaction where the engine supports it.

Files: `table_tab.rs`, `table_editor.rs`.

### 09 · Row menu & Copy as

Header line says what's selected ("1 row selected · orders #10356"). Copy,
**Copy as ›** (JSON, INSERT, UPDATE, CSV, TSV, Markdown, `IN (…)`, column
names), each with a mono example and a live clipboard preview on hover. Open
row details, Edit cell, Duplicate, Set NULL, Filter by this value, open
referenced rows, **Rows referencing this ›**. Delete row last, red, staged.

Files: `copy.rs`, `table_tab.rs`.

### 10 · Filter popover

Opened by `+ Filter` (⌘F) or a chip. all/any switch; one row per condition
(column picker, type-aware operator, value editor = that type's cell editor;
`between` shows two fields). The live `WHERE …` preview can be copied; **Edit
as SQL** swaps to one expression editor. ↵ applies, Esc closes.

Files: `table_tab.rs`.

### 11 · Commit confirmation

Shown by `guarded()` before writes when safe mode asks. A banner in the env
colour ("You are writing to PRODUCTION · connection · database"), the exact
SQL in run order with an affected-rows summary, Copy. The button label says
what happens and where ("Commit to prod"): red for production or destructive
DDL, otherwise accent. Focus starts on Cancel for production. An optional
10-minute snooze applies only to "Confirm writes", in memory only. The same
dialog serves Commit, Apply DDL, write queries, Import, Drop and Truncate.

Files: `mod.rs` (`guarded`).

### 12 · Export (⌘⇧E)

Format tiles (CSV, TSV, JSON, JSON Lines, Markdown, HTML, SQL INSERT), rows
scope (current filter / selection / whole table with counts), format options,
a 2-row live preview, destination path. Long exports continue in a progress
toast (rows done / total, ETA, Cancel).

Files: `copy.rs`, `export.rs`.

### 13 · Import CSV

File summary (rows, columns, size, encoding, delimiter); options (header
row, empty → NULL, skip failing rows); a **column mapping** table (CSV column →
table column or skip, 3 sample values, ✓ / ⚠ / ×) with local type validation
and row numbers for problems. Unmapped NOT NULL columns without a default
block Import. One transaction, through `guarded()`.

Files: `csv_import.rs`.

### 14 · EXPLAIN

Explain result tab: Tree / Raw, Analyze toggle (rolled-back transaction, as
today), Copy plan. Tree table with 22px indent per level, operation 12.5/600,
condition in mono; cost bar (accent, amber > 80%, red for the hottest leaf);
estimate → actual in amber when off by more than 10×. One plain-language
**insight** for the hottest node (e.g. a seq scan discarding most rows, with
Copy CREATE INDEX). Never more than one; hidden for cheap plans.

Files: `query_tab.rs`.

### 15 · History (⌘Y) & saved queries (⌘⇧O)

One dialog with two tabs, replacing two separate pickers. Search, all / ok /
failed filter; entries show time, status dot, SQL (2 lines), and rows · time
or the error in red. The preview pane has the full SQL plus Open in new tab,
Insert at cursor and Save as…. Saved queries are grouped by folder.

Files: `history.rs`, `saved.rs`, `session.rs`.

### 16 · Process list (⌘⇧P)

Live refresh every 2s with Pause; Active / Idle in tx / All filter. Columns:
PID, User, Database, State (pill), Duration (red over 1 min), Wait (Lock in
amber), Query. The detail strip shows the full query, client, start time and
**blocked by PID n** (clickable). Cancel query and Kill session (red) both go
through safe-mode confirmation; your own session is tagged and can't be
killed.

Files: `processes.rs`.

### 17 · Open connection (⌘⇧K / ⌘N)

Same look as the palette. Open connections first (with their ⌘n), then
saved by recency, then New connection…. ↵ opens or switches; ⌘↵ opens in the
background.

Files: `picker.rs`.

### 18 · New table

Name, **Start from** (Blank / id + timestamps / Copy of…), the same column
grid as Structure (08), a searchable type picker (enums and domains first),
Preview DDL, Create table.

Files: `table_editor.rs`.

### 19 · Redis

Sidebar becomes a **key tree**: a glob pattern fed to SCAN (never KEYS),
namespaces split on `:`, a type badge per key (hash blue, string green, list
amber, set violet, zset teal, stream red). The main area shows a key header
(type, key, size, **TTL** countdown, Rename, Delete) and a type-specific
editor: hash field/value grid, list index/value, set values, zset member/score,
string text (JSON auto-formatted), stream read-only. Edits stage and become
HSET / LSET / … on Save. The existing one-command-per-line console is docked at
the bottom.

### 20 · MongoDB

Collections in the sidebar (layers icon, doc count). Find bar with JSON
highlighting and field autocomplete; sort, projection and limit as chips.
Views: **Documents** (one card per document, default), **Table** (top-level
fields as columns), **JSON** (raw). Edit values in place; changes stage and
save as `$set` by `_id`.

### 21 · Settings (⌘,)

Sections: General, Appearance, Editor, Data grid, Safe mode, Shortcuts (a
searchable read-only list generated from the bindings in `mod.rs`). Segmented
control for ≤ 3 options, select for lists, toggle for booleans. Changes apply
instantly; no Save button.

Files: `settings_view.rs`, `settings.rs`.

### 22 · Empty, loading & error states

| State | Rule |
| --- | --- |
| Empty table | "`table` has no rows yet" + `+ Add row ⌘I` / Import CSV… |
| No filter matches | "No rows match 2 filters", restates the filters + counts; Edit / Clear filters |
| Loading | Skeleton rows after 150ms (never a spinner over a blank panel) + a small "Loading · 1.2 s · Stop" chip |
| Query error | Red block: message, line/column + SQLSTATE, a suggestion ("Did you mean total?") with a one-click fix, Copy error; the editor underlines the spot |
| Connection lost | Amber banner with countdown + Retry now; grid stays visible, dimmed, read-only; staged edits are kept |
| Could not connect | Centred: what failed and where (tunnel up / DB silent), Retry, Edit connection…, Details |
| Empty query tab | Faint hint comment in the editor; shortcut legend in the results area |

### 23 · Light theme

Screen 01 with every dark token swapped for its light twin, and nothing else
changed. **If a colour in code isn't a token, light mode will break.** ⌘⇧L
toggles (existing).

### 24 · Onboarding (first run and welcome back)

**Problem today:** nothing introduces the app, and macOS asks for Keychain
access before the user has done anything. The prompt comes from session
restore: at launch `AppView` calls `restore_state` (`src/ui/app.rs:71`), which
connects last session's connections, and `connect` reads their passwords with
`store::load_secrets` (`src/ui/home.rs:137`). A system dialog with no context
is the first thing a new or returning user sees.

**Rules**

1. **No keychain access until the user asks for something that needs it**:
   saving a password, connecting to a saved connection with a password, or
   importing connections with passwords. Never at launch, never during the
   first two onboarding steps.
2. **Explain before asking.** The first time Tabula is about to touch the
   keychain, show the "Passwords" step (24.4) first; the system prompt comes
   only after the user presses Continue.
3. **Launch never connects on its own.** With "Reopen last session" on,
   launch shows the Welcome back card (24.6) and connects only when the user
   presses Reopen (or ↵).
4. **Every step can be skipped**, and nothing in onboarding is required to
   use the app. "Skip setup" lands on Connections home (03) with defaults
   (System theme, 13px).
5. **Onboarding runs once.** New setting `onboarded: bool` (default `false`
   in `settings.json`). It's set when the user finishes or skips. Existing
   installs with saved connections are treated as onboarded and see only
   24.6.

**Flow**

```
first launch ─► 24.1 Welcome ─► 24.2 Look & feel ─► 24.3 Connections ─┬─► sample database ─► 24.5 You're set
                    │                                                   ├─► new connection ─► 24.4 Passwords* ─► 04 form ─► 24.5
                    └─ "Coming from TablePlus?" ────────────────────────┴─► import ─► 24.4 Passwords* ─► review ─► 24.5
later launches  ─► 24.6 Welcome back (only if there's a session to reopen) ─► 03 / workspace
* only the first time a password is involved; skipped for SQLite and the sample database
```

**Screens**

| # | Screen | Content | Actions |
| --- | --- | --- | --- |
| 24.1 | Welcome | Logo, "Welcome to Tabula", one-line pitch, three feature cards (Fast and native · Safe by default · Every database), a privacy line | **Get started**; "Coming from TablePlus? Import your connections" (jumps to 24.3 with Import pre-selected); Skip setup |
| 24.2 | Look & feel | Theme cards System / Dark / Light, each with a mini window preview (System shows half dark, half light); text size Compact 12 / Default 13 / Large 15 with a live preview row | Back · Continue. Changes apply live, so the next screens already use them |
| 24.3 | Connections | Four choices as large cards: **Import from TablePlus** (file or this Mac), **New connection**, **Open a SQLite file**, **Try the sample database** | Picking one continues; "I'll do this later" goes to 24.5 |
| 24.4 | Passwords | Why the keychain: passwords are stored only there (service "tabula", one item per connection), never in files or logs. An illustration of the macOS dialog with **Always Allow** highlighted. What happens if you choose Don't Allow | **Continue**, then the macOS prompt appears; "Don't save passwords" (Tabula asks for the password at each connect and stores nothing) |
| 24.5 | You're set | Short checklist of what was set up, plus the four shortcuts worth knowing (⌘K databases, ⌘P go to anything, ⌘↵ run, ⌘S commit) | **Open Tabula** |
| 24.6 | Welcome back | On Connections home: a card "Pick up where you left off?" listing last session's connections (env pills) and tab count | **Reopen** (↵), Not now, and an "Always reopen without asking" checkbox that restores the old automatic behaviour |

Board **24.4b** shows the three keychain outcomes side by side: Allowed
(auto-continues after 1.2 s), Not allowed / unavailable (with "How to fix"
steps, Try again, Continue anyway), and the password prompt used at connect
when passwords aren't saved (a "Remember in Keychain" checkbox, off by
default).

**Reopen setting:** `restore_sessions: bool` becomes a three-way
`restore_mode`: Ask (default) / Always / Never. Old values map true → Ask and
false → Never, so nobody who had it on gets the automatic keychain prompt
back without opting in.

**What Continue does on 24.4:** it performs a harmless read of the "tabula"
keychain service right away, so the macOS dialog appears while the
explanation is still on screen, then shows the 24.4b outcome. On Linux the
same step explains Secret Service (GNOME Keyring / KWallet). On Windows
(Credential Manager, no prompt) the step is skipped.

**Keychain states (24.4)**

- **Allowed:** a green check "Passwords will be saved in macOS Keychain",
  then continue.
- **Denied, or the keychain is locked / missing** (no Secret Service on
  Linux): amber note "Tabula can't use the keychain, so passwords won't be
  saved. You'll be asked when you connect." plus "How to fix" (open Keychain
  Access → find the "tabula" items → Access Control → allow Tabula), and
  Continue anyway. Connections without passwords keep working, which matches
  how `connect` already behaves.
- **macOS asks again after every update:** the app isn't signed with a
  stable identity yet (README: "Not notarized"), and macOS ties "Always Allow"
  to the code signature, so each new build looks like a different app.
  Onboarding can't fix that; signing every release with the same Developer
  ID does. Until then, 24.4 adds "macOS may ask again after updates."

**Copy rules:** say what Tabula stores and where, and never "we" (there's no
server). The privacy line on 24.1 is "No account, no tracking. Your
connections and passwords stay on this Mac." (Tabula has no telemetry or
update check; database traffic goes only to the servers you connect to.)

**Sample database (new):** a small bundled SQLite file (customers, orders,
products, about 2,000 rows) copied to the config folder on first use. It
needs no password and no network, so it shows the grid, staged edits and the
query editor in the first minute. It's listed on 03 like any connection and
can be deleted.

**Implementation notes:** first-run check in `AppView::new`
(`!settings.onboarded && no saved connections`). Onboarding is a full-window
view in place of `Home`. `restore_state` becomes "show 24.6"; the reconnect
moves to its Reopen button. `store::load_secrets` and `save_secrets` go
through one gate that shows 24.4 the first time (remember
`keychain_explained: bool` in settings). Screens: Penpot boards
`24 · Onboarding 1–6` and `24 · Onboarding 4b — Keychain outcomes`, each
with a spec sheet.

## Cell editors

Enter or typing starts editing (typing replaces the value). Tab / ⇧Tab saves
and moves, ↵ saves, Esc cancels. The editing cell has a 2px accent border and
draws above its neighbours; popovers open 8px below the cell (above it if
there's no room). Every edit is staged.

| # | Type(s) | Editor | Key rules |
| --- | --- | --- | --- |
| 1 | text, varchar(n), citext | In-cell input | Same font and padding as the cell; live `n / max` counter for varchar(n); ⌥↵ opens the long-text dialog |
| 2 | any | Validation error | Red 2px border + tooltip under the cell; ↵ / Tab blocked while invalid; server errors on Commit reuse it |
| 3 | char(n) | In-cell input | Input capped at n; hint that the server pads; pad spaces never shown in the grid |
| 4 | int, numeric, float, money | Right-aligned input | Raw value, no separators; ↑↓ ±1, ⇧ ±10; hint shows the stored form (`saved as 118.40`) |
| 5 | enum, MySQL ENUM, CHECK IN (…) | Dropdown | Existing `Choices` restyled: options as pills in declared order, ✓ on current, letter jumps, 1–9 picks, NULL last only if nullable |
| 6 | bool, tinyint(1), bit(1) | Toggle / 3-item dropdown | Grid shows a checkbox + literal; Space toggles directly; T / F / N pick |
| 7 | date, time, timestamp(tz), datetime | Input + picker | Text stays editable (ISO, "tomorrow 9am"); calendar, time, zone (tz only); `now()`, Today, NULL |
| 8 | foreign key | Search picker | Searches the referenced table by key and display columns (first text columns); 5 matches with context; invalid keys need a confirm |
| 9 | json, jsonb, xml, long text | 760px dialog | Replaces `edit_multiline`: Code / Tree, Format ⌘⇧F, live validity, Set NULL on the left; plain mode for text |
| 10 | uuid | Input + Generate | v4 client-side or the column default; normalises pasted forms |
| 11 | arrays, MySQL SET | Chip input | ↵ or `,` commits, ⌫ removes the last chip; suggestions from distinct values; accepts `{a,b}` on paste |
| 12 | NULL / DEFAULT / '' | Cell menu | NULL grey pill, DEFAULT violet pill, empty string faint `''`; ⌥⌫ sets NULL |
| 13 | bytea, blob, geometry, tsvector, custom | Menu / text | Binary shows icon + type + size, with Save / Load file and an image preview; unsupported types edit as text and the server casts |

**Display rules (not editing):** text mono left with `…`, newlines as `↵`;
integers and numerics right-aligned (separators for display only; none for
PK/FK); negatives red; bool checkbox + literal; enum pill with a colour hashed
from the value; dates ISO in the session zone; uuid middle-truncated; JSON
compact on one line; arrays as chips then `+N`; FK blue with `→`; binary icon
+ type + size; staged SQL functions (`now()`) violet.

## Keyboard

All existing bindings are kept. New ones are marked.

| Shortcut | Action |
| --- | --- |
| ⌘K | Database switcher (existing) |
| **⌘P** | **Command palette (new)** |
| ⌘N / ⌘⇧K | Open connection |
| ⌘1–⌘9, ⌘0 | Switch connection, home |
| ⌘T / ⌘W, ⌘⇧[ / ⌘⇧] | New / close tab, previous / next tab |
| ⌘↵, ⌘., ⌘E, ⌘⇧F | Run, stop, explain, format |
| ⌘Y, ⌘⇧S, ⌘⇧O | History, save query, saved queries |
| ⌘S, ⌘⇧⌫ | Commit, discard staged changes |
| ⌘F, ⌘I, ⌘⌫, ⌘R | Filter, add row, delete row, refresh |
| ⌘⇧I, ⌘⇧E, ⌘⇧P | Row panel, export, processes |
| ⌘,, ⌘⇧L, ⌘= / ⌘− | Settings, theme, zoom |
| **⌥⌫** | **Set cell to NULL (new)** |
| **⌥↵** | **Open the multi-line / JSON editor (new)** |
| **Space** | **Toggle a bool cell (new)** |
| **⌘D** | **Duplicate row (new; check for conflicts first)** |
| **/** | **Focus search on the home screen (new)** |

## Existing vs new

**Restyles of existing behaviour:** rail, sidebar tree, tabs, grid and staged
edits, enum dropdown, multi-line editor, row panel, Structure, New Table,
Copy as / Export, CSV import, EXPLAIN, history, saved queries, processes,
connection picker, database switcher, settings, Redis grouping, MongoDB
collections, safe-mode confirmation, light/dark theme.

**New proposals:** command palette (⌘P), filter chips and popover, editing
structure in place, cell editors for date / FK / UUID / array / binary,
validation-as-you-type, row-panel JSON and References tabs, Run on / row limit,
active-statement bar, read-only reason badge, EXPLAIN insight, merged
history / saved dialog, paste connection URL, inline Test connection, process
"blocked by", export progress toast, CSV column mapping, Redis typed editors
and TTL, MongoDB document cards, sectioned settings, all designed empty and
error states.

## Implementation plan

Suggested order: each step is shippable on its own and unblocks the next.

1. **Tokens first.** Define the colour tokens as two gpui-component
   `ThemeConfig`s (dark + light), so every `cx.theme()` call picks them up.
   Same JSON shape as gpui-component's `src/theme/default-theme.json`, loaded
   with `ThemeRegistry::load_themes_from_str`, or assigned to
   `theme.dark_theme` / `theme.light_theme`. Move `EnvTag::color` onto the env
   tokens and register Geist / Geist Mono. Nothing should look broken; it
   should just look like the new palette.
   - Today the app calls `Theme::sync_system_appearance` (`main.rs:106`),
     `Theme::change` on toggle (`app.rs:567`), and `Theme::global_mut` for font
     sizes (`settings.rs:77`) **without** calling `Theme::sync_base` afterwards.
     Use `Theme::update(cx, …)`, which keeps `colors`, `tokens` and the
     gpui-base copy in sync, or call `sync_base` after `global_mut`.
   - `Theme::change(mode)` reloads that mode's config, so colour or font edits
     made directly on the live theme are lost on ⌘⇧L. Put them in the configs.
   - Token → `ThemeColor` mapping (main ones): `bg/canvas` → `background`,
     `table`; `bg/sidebar` → `table_head`, `sidebar`; `bg/zebra` →
     `table_even`; `bg/hover` → `table_hover`, `list_hover`; `bg/raised` →
     `popover`; `bg/surface` → `input`, `secondary`; `border/default` →
     `border`, `sidebar_border`, `title_bar_border`, `status_bar_border`;
     `border/subtle` → `table_row_border`; accent at 12% → `table_active`,
     `selection`; `accent/primary` → `primary`, `ring`, `caret`,
     `table_active_border`; `text/primary` → `foreground`; `text/muted` →
     `muted_foreground`.
2. **Shell.** Title bar (breadcrumb, safe-mode badge), rail tiles, sidebar
   rows, tabs, status bar (screens 01–02 chrome).
3. **Grid + staged edits** (01): header, row states, pills, NULL / DEFAULT,
   pending-changes bar. This is the most-used surface.
4. **Cell editors** (07), starting with what exists (text, enum, multi-line)
   and adding bool, numbers, date, FK, JSON.
5. **Dialogs** on one shared dialog component: commit confirmation (11),
   export (12), connection form (04), settings (21).
6. **Query tab polish** (02, 14, 15).
7. **New features**, each behind its own PR: palette (05), filter popover
   (10), in-place structure (08), CSV mapping (13), Redis (19), MongoDB (20).
8. **States** (22) as each area is touched, not in one pass.

Every step needs the light theme checked; a non-token colour shows up there
first.

## Known gaps and open questions

- **Environment tags:** decided: four tags, Testing migrates to Development
  (see [Environment tags](#environment-tags)). The Penpot screens already
  match.
- **Not designed yet:** backup / restore, the SSH host-key trust prompt, the
  "Import from TablePlus" review flow, Create / Drop database dialogs, the
  schema popover (described in spec 06, not drawn), rename / duplicate / vacuum
  table dialogs.
- **Out of scope** (not supported by the app): ER diagram, users and roles,
  schema compare, view / function / trigger editors, multiple windows.

### Review questions

From a design review on 2026-11-06. Each one is also a comment thread in
Penpot (`#n`), pinned to the spot it's about. Answers below are **proposed**:
the ones marked *decided by code* follow what the app already does; the rest
need a yes/no before implementation.

**Mistakes in the mock-ups** (all to be fixed in Penpot)

- **02, stray text (#14):** a text layer "https://ramby.ai/plugin/manifest.json"
  sits next to the status bar. Delete it.
  → **Fix:** delete the layer. It isn't part of the design (it looks like a
  plugin URL pasted onto the canvas).
- **03, env filter (#15):** the filter shows All / Local / Staging /
  Production. Dev is missing from the four tags.
  → **Fix:** All / Local / Dev / Staging / Production.
- **17, open connections (#23):** 3 open connections are listed, but the rail
  shows 5 (redis-cache and events are missing).
  → **Fix:** list all five open connections (⌘1–⌘5), then Saved.
- **18, marker 3 (#24):** sits on the sidebar row "Failed payments", outside the
  dialog.
  → **Fix:** move it onto the dialog's Preview DDL button.
- **14, estimate colour (#21):** Seq Scan `9,800 → 21,330` is amber, but it's
  off by 2.2×. The rule says amber only when off by more than 10×.
  → **Fix:** Seq Scan estimate in normal text. Hash Join `1,420 → 18,912`
  (13×) stays amber; that one is correct.

**Rules that don't fit the app or the database**

- **01, safe-mode badge (#11):** `SafeMode` has five values (Auto, Off,
  ConfirmWrites, ConfirmAll, ReadOnly); the badge covers Off / Confirm /
  Read-only. What does it show for Auto (the resolved mode, e.g.
  "Auto · Confirm"?) and for ConfirmAll?
  → **Answer:** the badge shows the **effective** behaviour, with "Auto ·"
  when it comes from Auto:

  | Setting | Badge | Colour |
  | --- | --- | --- |
  | Auto on Production | `Auto · Confirm writes` | amber |
  | Auto elsewhere | `Auto · Off` | muted |
  | Off | `Safe mode off` | muted |
  | ConfirmWrites | `Confirm writes` | amber |
  | ConfirmAll | `Confirm every query` | amber, with a shield icon |
  | ReadOnly | `Read-only` | red, with a lock icon |

  The muted states stay visible (never hidden), so the badge is always where
  people look for it.
- **01, enum pill colours (#10):** `pending` is an amber pill, and amber also
  means "edited cell". If enum colours are hashed from the value, some values
  will land on amber, red or green and look like staged, deleted or new rows.
  Use an enum palette that excludes the three state colours?
  → **Answer: yes.** Enum pills hash into a separate **4-colour** palette that
  avoids every colour that already means something:
  - blue `#4FA3F7`, teal `#3CC7C2`, pink `#F07AC0`, slate `#94A3B8`
    (light theme: the darker variants);
  - not amber / red / green (edited / deleted / new rows);
  - not violet (`#B07CFF` is the DEFAULT pill and staged `now()` values, so
    a violet enum next to DEFAULT in a new row would read as the same thing);
  - not accent indigo (selection).

  Slate is close to the grey NULL pill, but an enum pill always has a dot and
  its value as text while NULL is the literal word in mono, so they stay
  distinguishable. Four colours means repeats on enums with more than four
  values; that's acceptable because the colour is a scanning aid, and the
  label is what carries the meaning. The mock-ups' `pending` (amber), `paid`
  (green), `refunded` (violet) and `cancelled` (red) pills get recoloured.
- **14, cost bars (#21):** with total cost, a parent is always at least as big
  as its children, so the root is always 100% and always amber. Use each
  node's own (exclusive) cost instead?
  → **Answer: yes.** The bar is the node's exclusive cost (its total minus
  its children's) as a share of the plan total. With Analyze on, it's
  exclusive time. Colours: accent normally, amber above 30% of the plan, red
  for the single largest node. The inclusive figure moves to the Cost column
  text (`self 62% · total 88%`).
- **13, skip failing rows (#20):** conflicts with "one transaction". On
  Postgres one failing row aborts the transaction, so skipping needs a
  SAVEPOINT per row (slow on big files). Proposal: "skip" covers only rows our
  local validation rejects; a server error still aborts the import.
  → **Answer: agreed.** The option is renamed **"Skip rows that fail
  validation"**, and the dialog says how many rows that is before Import. A
  server error rolls back everything and reports the row number.

**Decisions needed**

- **01, schema picker (#9):** it's in two places, the title-bar breadcrumb
  (`public ⌄`) and the sidebar footer select. Keep the breadcrumb and drop the
  footer?
  → **Answer: yes.** Keep only the breadcrumb (Postgres only); the sidebar
  footer goes. The sidebar still groups tables by schema when "All schemas"
  is chosen.
- **01, pending-changes bar (#8):** it covers the last two rows. Should the
  grid add bottom padding (bar height + 18px) while the bar is shown?
  → **Answer: yes.** While the bar is shown the grid gets 82px of extra
  scroll space at the bottom (46 bar + 18 gap + 18 margin), so every row can
  scroll clear of it. No layout jump: only the scrollable area grows.
- **01, checkbox column (#12):** new (today rows are selected by click /
  shift-click). Is it always shown? It takes about 40px from every table.
  → **Answer: drop it.** Today the grid selects one row at a time
  (`table_tab.rs`), and the row highlight + 2px accent bar already shows the
  selection. Multi-select (⌘-click / ⇧-click) is a separate feature; if it's
  added later, the context-menu header ("24 rows selected") shows the count.
  Remove the checkbox column from 01, 09, 11 and 23.
- **01, row panel width (#13):** with rail 60 + sidebar 260 + panel 340, a
  1280px window leaves about 620px for the grid. Overlay the panel below some
  window width?
  → **Answer: yes.** At window widths of 1440px and up the panel docks (as
  drawn). Below 1440px it floats over the grid's right edge with a shadow and
  doesn't resize the grid. The panel is resizable (280–480px) and remembers
  its width.
- **11, commit focus (#19):** focus starts on Cancel for production, but the
  footer says ⌘↵ commits. Does ⌘↵ still commit on prod? Also,
  "2 existing rows change" covers a delete; "1 updated · 1 deleted ·
  1 inserted" would be clearer.
  → **Answer:** ⌘↵ **does** commit on production. Focus on Cancel only
  protects against a bare ↵ (the most common accidental key), and ⌘↵ is a
  deliberate chord. Footer: "↵ cancels · ⌘↵ commits to prod". Summary line
  becomes **"1 updated · 1 deleted · 1 inserted"**.
  **Trade-off, accepted on purpose:** one chord commits to production. It's
  still two deliberate steps (⌘S opens this dialog, then ⌘↵ confirms), and
  the dialog always shows the env banner and the SQL first. If that proves too
  easy, the fallback is to make production require a click on the button
  (⌘↵ ignored when the env is Production) without changing anything else.
  *Owner to confirm.*
- **10, operators (#18):** `< ≤ > ≥` is a single menu item. How is one of the
  four picked? Text operators (contains, starts with, LIKE / ILIKE) and
  `not in` are missing.
  → **Answer:** each operator is its own item, grouped by type:
  - all types: `=`, `≠`, `in`, `not in`, `is null`, `is not null`;
  - numbers and dates: `<`, `≤`, `>`, `≥`, `between`;
  - text: `contains`, `starts with`, `ends with` (case-insensitive via ILIKE
    / LOWER, with an "Aa" toggle for case-sensitive), `LIKE`, `~ regex`
    (Postgres / MySQL REGEXP);
  - json: `has key`, `->> path =`.

  Typing filters the operator list.
- **09, ⌘D duplicate row (#22):** is the primary key left as DEFAULT, or
  copied (which fails on commit)?
  → *Decided by code:* the existing `duplicate_row` (`table_tab.rs:625`)
  leaves primary-key columns empty, so they insert as DEFAULT, and copies
  everything else. Keep that. Also leave identity / serial /
  `gen_random_uuid()`-default columns as DEFAULT. Copy other UNIQUE columns
  but mark them amber, so they're fixed before Commit.
- **19, Redis (#25):** is Delete key staged with the other edits, or immediate
  with a confirmation? Large keys (a list or zset with 1M members) need paging
  (LRANGE / HSCAN / ZSCAN pages). Where does the pager go?
  → **Answer:** Delete key is **staged** like every other change: the key
  shows struck-through in red in the tree and is deleted on Save, through
  `guarded()`. Paging sits in a footer under the field grid, above the
  console:
  - lists and zsets (index-addressable): `1–200 of 1,048,576 ‹ ›` with a page
    size of 200;
  - hashes and sets (cursor-based HSCAN / SSCAN): `200 loaded · Load more`
    with the total from HLEN / SCARD.
- **20, MongoDB (#26):** the documents view has no pager, and the status bar
  says "50 of 1.8M docs". Where are next page and page size?
  → **Answer:** on the right of the find bar, next to the view switch:
  `1–50 of ~1.8M ‹ ›` plus a page-size select (25 / 50 / 100 / 500).
  - **Paging is keyset, not skip.** `skip` makes the server walk every
    skipped document, which gets slow on deep pages of a 1.8M-document
    collection. ‹ and › page by the last / first sort key: with the default
    sort, `_id > last_id` + `limit`. With a custom sort, use (sort field,
    `_id`) as a compound key so ties don't drop documents.
  - **No "jump to page n".** Keyset paging can't seek to an arbitrary page,
    so the pager offers first, previous and next only. To get further, add a
    filter, which is faster than paging anyway. `skip` is kept only as a
    fallback when the sort field can't be compared (e.g. mixed types), and
    then only for the first 10 pages.
  - **Count:** without a filter the total uses `estimatedDocumentCount`
    (hence "~"). With a filter it uses `countDocuments` with a 2-second
    timeout, and shows "50+" if that times out.

  This changes existing code: Mongo paging today is `.skip(q.offset)`
  (`src/db/mongo.rs:348`). SQL table paging is also offset-based (the
  `offset` field in `src/db/mod.rs:401`). That's fine for the first pages but
  has the same deep-page cost on big tables, so the same keyset rule is worth
  applying there later (by primary key).

## Feasibility notes (checked against the code)

Everything in the redesign fits gpui-component 0.9 without forking it. Details
and the catches:

- **Grid header (01):** `TableDelegate::render_th` (gpui-component
  `src/table/delegate.rs:102`) renders any element per column, so the two-line
  name + mono type + key icon header goes there. The app only implements
  `render_td` today (`src/ui/grid.rs:333`). Header height:
  `DataTable::header_row_height(px(40.))`.
- **Row and cell states (01, 08):** `render_tr` returns the row `Div`, and its
  style is refined over the defaults (`src/table/state.rs:2257`), so the tint
  and 2px left bar for new / deleted / selected rows work. `render_td` is
  fully custom, so the edited-cell fill and border go there (use
  `size_full()`). **Catch:** the library paints hover after your style, so
  hover covers the tint. Set `table_hover` to a translucent colour, or accept
  it.
- **Row height and zebra:** already 32px-capable via `Size::Size(px)` (the app
  derives it from the UI font size in `grid.rs:28`; pin it to 32 or keep the
  ratio). Zebra is `.stripe(true)` (already on) using `table_even`.
- **Shortcuts:** ⌘P and ⌘D are free everywhere (app, gpui-component,
  gpui-base). ⌥↵ is unbound but should still be scoped to the grid.
  **Catches:**
  - ⌥⌫ is Input's "delete previous word", so scope Set-NULL to
    `"EditableGrid > DataTable"` or `"!Input"`, like `sec!("backspace")` in
    `mod.rs`.
  - **Space** won't reach a binding: the grid's type-to-edit handler
    (`cell_edit.rs:396`, `capture_key_down`) starts an edit on any printable
    key, including space, before bindings run. Bool toggling needs a special
    case inside that handler.
- **Theme, fonts, icons:** see [Implementation plan](#implementation-plan)
  step 1, [Typography](#typography) and [Icons](#icons).
