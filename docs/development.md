# Building and testing

## Requirements

- **Rust:** the version pinned in `rust-toolchain.toml`. rustup installs it
  automatically on the first `cargo` command.
- **macOS:** nothing else. Shaders are compiled at startup
  (`runtime_shaders`), so Xcode's Metal toolchain isn't needed.
- **Debian / Ubuntu:**

    ```sh
    sudo apt-get install libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev \
      libx11-xcb-dev libxcb1-dev libfontconfig-dev libfreetype-dev libzstd-dev libdbus-1-dev \
      libssl-dev libasound2-dev pkg-config clang cmake
    ```

- **Windows:** the MSVC build tools (Visual Studio "Desktop development with
  C++").

## Build and run

```sh
cargo run              # debug build
cargo run --release    # optimized; much faster grids and startup
```

The binary is `target/release/tabula` (`tabula.exe` on Windows).

## Tests

```sh
cargo test                  # unit tests, SQLite end-to-end, UI tests
TABULA_IT=1 cargo test      # also against real Postgres, MySQL, Redis, MongoDB
```

There are four kinds of test:

- **Unit tests** sit at the bottom of most files: the SQL splitter,
  formatter, safety check, EXPLAIN parsers, export formats, CSV import,
  TablePlus import, RNCryptor, storage, and so on.
- **SSH tests** run against an in-process SSH server (`db/ssh.rs`).
- **UI tests** (`ui/app.rs`, with gpui's `test-support`) open the real app
  in a headless window, dispatch keystrokes and actions, and check the
  views. Examples: run a query with ⌘↵, cancel it with ⌘., edit and commit
  a cell, switch database with ⌘K, restore a session.
- **End-to-end tests** (`db/it.rs`) exercise every driver against a real
  server. SQLite always runs. The others need `TABULA_IT=1` and these
  throwaway containers:

```sh
docker run -d --rm --name tabula-test-pg -e POSTGRES_PASSWORD=test -p 55432:5432 postgres:17-alpine
docker run -d --rm --name tabula-test-mysql -e MYSQL_ROOT_PASSWORD=test -e MYSQL_DATABASE=app -p 53306:3306 mysql:8.4
docker run -d --rm --name tabula-test-redis -p 56379:6379 redis:7-alpine
docker run -d --rm --name tabula-test-mongo -p 57017:27017 mongo:7
docker exec tabula-test-pg createdb -U postgres tabula_it_other
```

With `TABULA_IT_PAUSE=1` as well, two more tests `docker pause` the Redis
and Postgres containers to check that a server that stops answering times
out with a clear error and recovers.

Tests never touch your real data: `store::config_dir()` points to a temp
folder under `cargo test`, and the keychain isn't used.

## Conventions

- **Errors are values.** Return `anyhow::Result` and add context; don't
  `unwrap` on anything that depends on input, files, the network or a
  server. A panic on the tokio side becomes an "internal error" message,
  but a panic on the UI thread ends the app.
- **No re-entrant updates.** Code that might run inside another update of
  the same entity (shortcut handlers, dialog callbacks, notifications from
  settings) defers with `window.defer` / `cx.defer`.
- **Stale results are dropped.** Async UI work carries a sequence number
  (`load_seq`, `edit_gen`) and ignores results from older requests.
- **Database work runs on tokio** via `runtime::spawn`. Never block the UI
  thread on I/O.
- **Writes go through safe mode** (`db::safety` + `ui::guarded`).
- **Secrets stay in the keychain.** Nothing secret goes into JSON files,
  logs or command lines.
- **Comments** explain *why*, briefly. Each file starts with a `//!`
  comment describing what it does.

## Editing these docs

The site is built with [MkDocs Material](https://squidfunk.github.io/mkdocs-material/)
from `docs/` and `mkdocs.yml`. To preview locally:

```sh
python3 -m venv .venv
.venv/bin/pip install -r docs/requirements.txt
.venv/bin/mkdocs serve      # http://127.0.0.1:8000, reloads on save
```

Pushing changes under `docs/` (or `mkdocs.yml`) to `main` republishes the
site (see [Releases and CI](releases.md#documentation-site-docsyml)).
