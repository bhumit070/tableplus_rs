# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Tabula (crate `tabula`, repo `tableplus_rs`) is a native TablePlus-style database client for PostgreSQL, MySQL, SQL Server, SQLite, Redis and MongoDB, written in Rust (edition 2024, toolchain pinned in `rust-toolchain.toml`) with gpui (`gpui-pre`) and gpui-component 0.9. Full developer docs live in `docs/` (MkDocs site); `docs/architecture/*.md` and `docs/modules.md` go deeper than this file.

## Commands

```sh
cargo build                      # keep it warning-free
cargo run [--release]            # release is much faster for grids/startup
cargo fmt                        # rustfmt defaults; enable hook once: git config core.hooksPath .githooks
cargo test                       # unit + SQLite end-to-end + headless UI tests
cargo test <name>                # single test by name substring

# End-to-end against real servers (throwaway containers from compose.yaml, ports 55432/53306/51433/56379/57017)
docker compose up -d --wait
TABULA_IT=1 cargo test
docker compose down

# Pause tests: must run on their own
TABULA_IT=1 TABULA_IT_PAUSE=1 cargo test frozen_server_times_out_and_recovers
TABULA_IT=1 TABULA_IT_PAUSE=1 cargo test sidebar_shows_load_errors

# Offscreen screenshots (macOS, dark + light) -> target/shots/*.png; scenarios in src/ui/app/shots.rs
cargo test screenshots -- --ignored --nocapture

python3 assets/themes/tabula.py  # regenerate assets/themes/tabula.json from colour tokens
```

**Keep `target/` small.** Repeated builds can push it past 100 GB on the user's Mac. Before starting a new build, remove the old artifacts. If you're switching to the other profile, clean the one you're leaving: `cargo clean --release` when going back to debug, `cargo clean --profile dev` when moving to release. Check `du -sh target` and run a full `cargo clean` when it gets large. Don't leave stale builds behind.

Tests never touch real user data: `store::config_dir()` points to a temp dir under `cargo test`, and the keychain isn't used. UI tests live in `src/ui/app.rs` `mod tests` (drive with `simulate_keystrokes`, clicks via `debug_bounds("selector")` + `simulate_click`). Driver end-to-end tests are in `src/db/it.rs`; SSH tests use an in-process server in `src/db/ssh.rs`.

## Architecture

Three layers:

- **UI (`src/ui/`, gpui main thread):** `AppView` (`ui/app.rs`, window root, title bar, connection rail, global action routing) → `Home` or one `Session` per open connection (`ui/session.rs`, sidebar + tabs) → `TableTab` / `QueryTab`, each rendering a `Grid` (`ui/grid.rs`, the `TableDelegate` for every data grid).
- **App services (`src/*.rs`):** `store` (connections + keychain), `settings`, `restore` (`session.json`), `history`, `saved`, export/import, `rncryptor` (TablePlus export decryption), `mcp` (the optional MCP server for AI tools: rmcp Streamable HTTP on 127.0.0.1 behind a bearer token; writes go through the same safe-mode gates, and `Confirm` asks the window via `ui/mcp_service.rs`).
- **Database (`src/db/`, tokio):** one `Connection` type (`db/mod.rs`) fronting `sql.rs` (Postgres/MySQL/SQLite via sqlx), `mssql.rs` (SQL Server via tiberius, own pool, T-SQL `GO` batches), `redis_kv.rs`, `mongo.rs`, optionally over an SSH tunnel (`ssh.rs`, russh). Also SQL splitting, formatting, `safety`, EXPLAIN parsing, DDL, cancel, and `target` (detects single-table queries so results can be edited).

Key cross-cutting mechanics:

- **Two executors.** gpui owns the main thread (entities are `!Send`); DB drivers run on a separate tokio runtime (`src/runtime.rs`). UI calls `runtime::spawn(fut)` and awaits the result from `cx.spawn`; panics on the tokio side become an error, panics on the UI thread kill the app. Never block the UI thread on I/O.
- **Stale results are dropped.** Async UI requests carry a sequence number (`load_seq` in `TableTab`, `edit_gen` in `QueryTab`); late results from older requests are ignored.
- **Shared connection.** A session and its tabs share `ui::Shared { cfg, conn: Arc<RwLock<Connection>>, schema }`. Reads/queries/commits take the read lock; switching database or Postgres schema takes the write lock (3 s wait, then asks the user to stop running work).
- **Everything is text.** Drivers return `QueryResult { columns, rows: Vec<Vec<Option<String>>> }` via the text protocol (`raw_sql`); writes use bound parameters. Redis/Mongo map into the same shape so grid, export, copy and row panel are driver-agnostic.
- **Staged editing.** `Grid` layers `edits`, new rows (past `base_rows`) and `deleted` on top of fetched rows; commit turns them into `db::Change` (`Update`/`Insert`/`Delete`, keyed by PK / `_id` / Redis key) run in one transaction. Inline editing is shared through the `CellEditHost` trait (`ui/cell_edit.rs`); typed validation in `ui/cell_value.rs`.
- **Safe mode.** Every write goes through `ui::guarded(gate, …)` with a gate from `db::safety` (`Run` / `Confirm` / `Block`). Commit confirmation UI is in `ui/confirm.rs`.
- **Background reads time out** after 30 s via `bounded()` in `db/mod.rs`; editor queries have no timeout and are stopped with Cancel (`db/cancel.rs`, cancels server-side).
- **Actions/shortcuts.** All commands are gpui actions declared in `src/ui/mod.rs`; `key_bindings()` maps keys (`sec!` = ⌘ on macOS, Ctrl elsewhere). Most bindings have no context so they work everywhere; keys that must differ in text fields use `!Input`. Global handlers in `ui::app::register_actions` find their target via `with_app` / `with_session` / `with_table`. The menu bar in `src/main.rs` lists the same actions.
- **Theme.** Colours come from `cx.theme()` (Tabula Dark/Light from `assets/themes/tabula.json`), never hex literals. Use `theme::dp(px)` for design pixels (13px base); rems follow the interface font size.

## Conventions

- Import everything with `use` at the top of the file (or of its `mod tests` for test-only items) and refer to it by its short name. Don't write crate paths inline, even though Rust 2018+ allows it: `use gpui_platform::application;` then `application()`, not `gpui_platform::application()`; `use serde_json::Value;` then `Value::Null`. A bare `use gpui_platform;` doesn't count — import the items. On name clashes, alias the import (`use sqlx::Error as SqlxError;`, `use tokio::spawn as tokio_spawn;`). Exceptions: attribute macros like `#[tokio::test]` / `#[gpui::test]` (importing `test` would shadow the built-in `#[test]`) and `anyhow::Ok` (it would shadow `Ok`).
- Return `anyhow::Result` with context; don't `unwrap` anything depending on input, files, network or a server.
- Code that may run inside another update of the same entity (shortcut handlers, dialog callbacks, settings notifications) must defer with `window.defer` / `cx.defer`, or gpui panics ("already being updated").
- Secrets stay in the keychain — never in JSON files, logs or command lines. `*.tableplusconnection` files contain real passwords: never read or commit them.
- Each file starts with a `//!` comment describing what it does; comments explain *why*, briefly.
- **Small commits.** Commit after each small change once it's verified
  (builds with no warnings, `cargo test` passes, screenshots checked for UI
  changes), so every commit works on its own and nothing is left broken
  between them. Don't batch unrelated changes into one commit.
- **Descriptive names.** Name variables, closures and parameters for what
  they hold (`table_name`, `on_pick`, `selected_row`), not `a`, `b`, `x`,
  `t2`, so the code reads and debugs easily. Single letters are fine only
  for tiny conventional scopes (`|i|` as an index in a short closure).
- Keep docs in step with behaviour changes: `README.md`, `docs/` (features, shortcuts, architecture, modules, development).

## gpui / gpui-component gotchas

- Key bindings run before key-down listeners. To override an input's own keys (↑/↓, Esc, Tab) in the cell editor, bind actions in the `"CellEditor > Input"` context (`src/ui/mod.rs`); `capture_key_down` won't work.
- macOS marks arrow keys with the `function` modifier; don't use `Modifiers::modified()` to mean "no modifiers".
- `DataTable` calls `render_tr` for filler rows past the data — check `row_ix < rows.len()`.
- `IconName` must come from `gpui_kit_assets` (full Lucide), not gpui-component's subset.
- `impl IntoElement` return types capture `cx`; return `AnyElement` when the result is used alongside further `cx` borrows.

## Releases — ask before pushing

Every push to `main` touching `src/**`, `Cargo.toml`, `Cargo.lock` or `rust-toolchain.toml` cuts a release (`.github/workflows/release.yml`) and updates the Homebrew tap and apt repo; pushes touching `docs/` republish the docs site. Never push without explicit approval for that push.

## In-progress work

A UI redesign is underway. `design.md` is the spec (tokens, screens 01–23, cell editors, implementation plan); `handover.md` tracks completed steps and what remains. Read both before UI work, and check UI changes with the screenshot test in both themes.
