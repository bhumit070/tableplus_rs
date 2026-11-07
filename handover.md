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

Screenshots now also cover 02-query-results, 04-edit-connection,
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
7. New features: full ⌘P palette with scope chips (a basic picker exists),
   filter popover (10) with per-type operators (answer #18), in-place
   Structure editing (08), CSV column mapping (13), Redis typed editors and
   TTL (19), MongoDB document cards and pager (20), process list restyle (16),
   open-connection picker restyle (17), new-table dialog (18), row menu with
   Copy as submenu and preview (09), row panel tabs Fields / JSON /
   References.
8. States (22): done except the **connection lost** banner (countdown,
   Retry, dimmed read-only grid, edits kept) and the query error
   **suggestion** ("Did you mean total?" with a one-click fix, SQLSTATE,
   line/column).

Smaller leftovers: array chip editor and binary save/load (cell editors #11,
#13); sidebar approximate row counts and Saved queries / Functions sections;
the 2px left bar on deleted / new rows is hidden under the row-header column;
design.md "Review questions" decisions not yet applied (e.g. row panel
overlay below 1440px, checkbox column dropped — already not present).

Keep docs in step: README.md, `docs/` (features, shortcuts, architecture/ui,
modules, development). The docs site deploys on push.
