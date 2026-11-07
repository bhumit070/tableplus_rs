# Storage and settings

Tabula keeps a few small files in its config folder and every password in
the operating system's credential store.

## Config folder

`store::config_dir()` is the platform's config directory plus `tabula`:

| Platform | Folder |
| --- | --- |
| macOS | `~/Library/Application Support/tabula/` |
| Linux | `~/.config/tabula/` (or `$XDG_CONFIG_HOME/tabula/`) |
| Windows | `%APPDATA%\tabula\` |

| File | Written by | Contents |
| --- | --- | --- |
| `connections.json` | `store.rs` | Saved connections, **without** passwords. |
| `settings.json` | `settings.rs` | Font sizes, session restore on/off. |
| `session.json` | `restore.rs` | Open connections, their tabs, unsaved query text. |
| `saved_queries.json` | `saved.rs` | Saved queries, each for one connection or for all. |
| `history.db` | `history.rs` | Query history (SQLite), up to 5,000 entries per connection. |
| `known_hosts` | `db/ssh.rs` | SSH host keys you chose to trust in Tabula. |

Under `cargo test`, `config_dir()` points to a throwaway temp folder, so
tests can never touch your real connections or settings.

## Writing files safely

- **Atomic writes:** `store::write_atomic` writes to a `.tmp` file, flushes
  it to disk (`sync_all`), then renames it over the original. A crash or
  power cut leaves either the old file or the new one, never half a file.
- **Damaged files are kept:** `store::read_json` reads every JSON file. If
  a file can't be parsed (hand edit, written by a newer version, …), it's
  moved aside (`connections.json.unreadable-<timestamp>`) and the user is told where. The
  next save then starts a fresh file instead of destroying the old one.
- **Unreadable files are protected:** if a file exists but can't be read
  (permissions, …), its path goes on a `PROTECTED` list and every write to
  it is refused for the rest of the run. An empty default never replaces
  data that's still there.
- **History** has a schema version (`pragma user_version`). A `history.db`
  from a newer Tabula turns history off instead of corrupting it. History
  is recorded in the background, so a slow disk never delays a query.

## Passwords: the OS keychain

`store.rs` uses the [`keyring`](https://crates.io/crates/keyring) crate,
which stores secrets in:

- **macOS:** the login Keychain;
- **Windows:** Credential Manager;
- **Linux:** the Secret Service (GNOME Keyring, KWallet).

Tabula touches the keychain only when a password is involved (connecting
to, editing, duplicating or importing a connection that isn't SQLite), never
at launch. The first such time it shows the "Passwords" step
(`ui/passwords.rs`) and records `keychain_explained` in `settings.json`.
With `save_passwords` off, `home.rs` neither reads nor writes the keychain;
passwords typed in the form stay in memory for that run.

Each connection has up to two entries under the service name `tabula`:

| Account | Secret |
| --- | --- |
| `{connection id}` | Database password. |
| `{connection id}:ssh` | SSH password or private-key passphrase. |

The rules:

- Secrets are read **only when connecting** (`load_secrets`), never at
  launch. SQLite connections skip the keychain entirely.
- Only "no entry" means "no password". Any other keychain failure (locked,
  access denied, no Secret Service running) is an **error**, never an empty
  password. Otherwise, saving the edit form afterwards could replace the
  real stored password with a blank. If the keychain fails, Tabula still
  tries to connect without a password, and says the keychain failed.
- Saving an empty secret deletes its entry. Deleting a connection deletes
  both of its entries.

## Settings

`settings.rs` holds `Settings { ui_font_size, editor_font_size,
restore_sessions }` as a gpui global. `settings::update` changes it,
applies it to the theme and saves the file. If saving fails, the
change still applies for now and a notification says it won't survive a
restart.

## Session restore

`restore.rs` defines what `session.json` holds: for each open connection,
its id, its tabs (`Table {schema, name, kind}` or `Query {title, text}`) and which
tab was in front. See [User interface](ui.md#session-restore) for when it's
written and read.
