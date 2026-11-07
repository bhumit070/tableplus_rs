# Safety and security

## Safe mode

Each connection has a safe mode (`db::SafeMode`), set in the connection
form:

| Mode | Behavior |
| --- | --- |
| **Auto** (default) | Like *Confirm writes* on connections tagged **Production**, like *Off* everywhere else. |
| **Off** | Nothing asks. |
| **Confirm writes** | Asks before running editor text that writes, and before every commit, import, truncate or drop. |
| **Confirm all** | Asks before running any query, and before every write. |
| **Read-only** | Refuses writes, in the app **and on the server**. |

`db::safety` makes the decision, and `ui::guarded` acts on it (see
[User interface](ui.md#safe-mode-in-the-ui)):

- `gate_query(cfg, text)` is used for editor text;
- `gate_write(cfg)` is used for grid commits, CSV import, truncate and drop.

Each returns `Run`, `Confirm` or `Block(reason)`. `ui::guarded` acts on
it: `Confirm` opens `ui/confirm.rs` (environment banner, the exact SQL,
a summary of what changes) unless *Confirm writes* is snoozed for that
connection (ten minutes, kept in memory only, never saved).

### Does this text write?

`is_read_only(driver, text)` errs on the side of "writes": anything it
can't positively identify as a read counts as a write.

- **SQL:** the text is split into statements. Comments are removed, with
  quotes respected, so `'--'` inside a string can't hide the rest of the
  line. A statement reads only if it starts with
    - `SELECT`, `VALUES` or `TABLE` without `INTO` (`SELECT … INTO` creates
      a table);
    - `SHOW`, `DESCRIBE` or `DESC`;
    - `WITH` with no `INTO` and no data-changing keyword (`WITH x AS
      (DELETE … RETURNING *) SELECT …` writes);
    - `EXPLAIN` without `ANALYZE` (which runs the statement);
    - `PRAGMA` without `=`.
- **Redis:** every line's command must be on a list of read commands
  (`GET`, `HGETALL`, `SCAN`, …).
- **MongoDB:** the method must be a read (`find`, `countDocuments`, …).
  `aggregate` counts as a read unless it uses `$out` or `$merge`. Raw
  command documents always count as writes.

### Enforced by the server

The text check is a convenience. Read-only mode doesn't rely on it: the
database session itself is made read-only (see
[Database layer](database.md#sql-postgresql-mysql-sqlite-sqlrs)). Even a write the
check missed is refused by Postgres, MySQL or SQLite.

!!! note
    Redis and MongoDB have no read-only session setting, so for them,
    read-only mode relies on the command check above.

### Other safeguards

- **Preview SQL** shows the exact statements a commit will run.
- Grid commits are **one transaction** on SQL databases. MongoDB without a
  replica set can't do that, and the UI warns that a failed commit may
  have saved some changes.
- Saving edits to a query result re-runs **only** that result's statement,
  and only if it reads. The rest of the batch isn't repeated.
- Switching database drops staged edits in query results, so they can't
  be saved to a same-named table in the other database.
- Dropping a database requires switching away from it first.

## Security

### Passwords

- Passwords live only in the OS keychain (see
  [Storage](storage.md#passwords-the-os-keychain)), never in Tabula's
  files.
- Backup and restore tools get the password through `PGPASSWORD` or a MySQL
  options file created with mode `0600` and deleted afterwards. It's never
  on the command line, where other users could see it in the process list.
- TablePlus export files are decrypted in memory (RNCryptor v3:
  PBKDF2-SHA1, AES-256-CBC, HMAC-SHA256, checked before decrypting).

### Network

- **TLS:** *Require encrypted connection* makes TLS mandatory. *Verify
  server certificate* also checks the certificate chain and host name
  (`verify-full` / `VERIFY_IDENTITY`). Without verification, the traffic is
  encrypted but an impostor server could still receive the password, so
  verifying is recommended. TLS is rustls; no OpenSSL is involved.
- **SSH host keys** are verified before login; changed keys are refused
  (see [SSH tunnels](ssh.md#host-keys)).

### Local data

- Query history stays on this machine (`history.db`).
- Tabula sends nothing anywhere except to the databases and SSH servers you
  connect to. There is no telemetry.
