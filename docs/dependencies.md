# Dependencies

Every crate in `Cargo.toml`, grouped by job, with why Tabula uses it. The
exact versions are in `Cargo.toml` and `Cargo.lock`.

## UI

| Crate | Used for |
| --- | --- |
| [`gpui`](https://crates.io/crates/gpui-pre) (package `gpui-pre`) | The UI framework from the Zed editor: GPU rendering (Metal, Vulkan, DirectX), entities, actions, key bindings, windows and menus. `gpui-pre` is a published snapshot of Zed's in-repo crate. |
| [`gpui_platform`](https://crates.io/crates/gpui-pre-platform) (package `gpui-pre-platform`) | gpui's per-OS backends, plus `application()`. Features: `font-kit` (system fonts), `runtime_shaders` (macOS compiles shaders at startup, so building needs no Xcode Metal toolchain), `x11` and `wayland` (Linux). |
| [`gpui-component`](https://crates.io/crates/gpui-component) | Widgets: inputs, the code editor with tree-sitter SQL highlighting, the virtualized table, dialogs, menus, notifications, title bar, theme. |
| [`gpui-kit-assets`](https://crates.io/crates/gpui-kit-assets) | The icon set gpui-component's `IconName` refers to. |
| [`lsp-types`](https://crates.io/crates/lsp-types) | gpui-component's completion API speaks LSP types (`CompletionItem`, `TextEdit`); used by autocomplete. |

## Databases

| Crate | Used for |
| --- | --- |
| [`sqlx`](https://crates.io/crates/sqlx) | PostgreSQL, MySQL and SQLite: pools, the text protocol for reads, bound parameters for writes, rustls TLS. Also stores query history (`history.db`). Default features are off; only the needed ones are on. |
| [`redis`](https://crates.io/crates/redis) | Redis client with tokio support and `ConnectionManager` (reconnects by itself). |
| [`mongodb`](https://crates.io/crates/mongodb) | The official MongoDB driver; BSON comes with it. |
| [`russh`](https://crates.io/crates/russh) | Pure-Rust SSH client (and server, in tests) for tunnels, with the `ring` crypto backend, RSA keys and compression. Its `keys` module reads private keys, `known_hosts` and the ssh-agent. |
| [`rust_decimal`](https://crates.io/crates/rust_decimal), [`chrono`](https://crates.io/crates/chrono) | Turned on as sqlx features (`rust_decimal`, `chrono`) for numeric and date types. Tabula's own code doesn't use them directly. |

## Async and runtime

| Crate | Used for |
| --- | --- |
| [`tokio`](https://crates.io/crates/tokio) | The runtime the drivers run on (`src/runtime.rs`), plus timeouts, locks (`RwLock` around each connection), `select!` for cancel, and `process` to run `pg_dump` and friends. |
| [`futures`](https://crates.io/crates/futures) | Stream helpers (`TryStreamExt`) and `BoxFuture` for the server-side cancel callback. |

## Data formats

| Crate | Used for |
| --- | --- |
| [`serde`](https://crates.io/crates/serde), [`serde_json`](https://crates.io/crates/serde_json) | Every JSON file Tabula writes, JSON export / copy, MongoDB values, EXPLAIN JSON from Postgres, TablePlus exports. |
| [`json5`](https://crates.io/crates/json5) | Parses mongosh-style arguments (`{age: {$gt: 30}}`: unquoted keys, single quotes). |
| [`plist`](https://crates.io/crates/plist) | Reads TablePlus's `Connections.plist` on macOS. |
| [`uuid`](https://crates.io/crates/uuid) | Ids for connections and saved queries; unique temp file names. |
| [`dirs`](https://crates.io/crates/dirs) | The per-OS config folder, home folder, and TablePlus's data folder. |
| [`anyhow`](https://crates.io/crates/anyhow) | Error type used everywhere, with context added on the way up. |

## Credentials and crypto

| Crate | Used for |
| --- | --- |
| [`keyring`](https://crates.io/crates/keyring) | Stores passwords in the OS credential store: Apple Keychain, Windows Credential Manager, Linux Secret Service. |
| [`aes`](https://crates.io/crates/aes), [`cbc`](https://crates.io/crates/cbc), [`pbkdf2`](https://crates.io/crates/pbkdf2), [`hmac`](https://crates.io/crates/hmac), [`sha1`](https://crates.io/crates/sha1), [`sha2`](https://crates.io/crates/sha2) | RNCryptor v3 decryption of TablePlus exports: AES-256-CBC, PBKDF2-HMAC-SHA1 key derivation, HMAC-SHA256 check. `sha2` also gives imported connections stable ids, so importing twice doesn't duplicate them. |

## Tests only (`[dev-dependencies]`)

| Crate | Used for |
| --- | --- |
| `gpui` with `test-support` | Headless UI tests: open the app in a test window, dispatch actions, read views. |
| [`csv`](https://crates.io/crates/csv) | Reads exported CSV back in tests, as an independent check of the export format. |
| [`rand`](https://crates.io/crates/rand) | Random SSH keys for the test SSH server. |

## Packaging (CI only)

| Tool | Used for |
| --- | --- |
| [`cargo-deb`](https://crates.io/crates/cargo-deb) | Builds the Linux `.deb` from `[package.metadata.deb]` in `Cargo.toml`. |
| [WiX Toolset 5](https://wixtoolset.org) | Builds the Windows `.msi` from `packaging/windows/tabula.wxs`. |
| `hdiutil` | Builds the macOS `.dmg` (part of macOS). |
| [MkDocs Material](https://squidfunk.github.io/mkdocs-material/) | Builds this documentation site. |
