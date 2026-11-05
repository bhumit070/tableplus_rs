# Tabula

Tabula is a fast, native database client in the spirit of
[TablePlus](https://tableplus.com), written in Rust with
[gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui) (the UI
framework behind the Zed editor) and
[gpui-component](https://github.com/longbridge/gpui-component). There is no
limit on open connections: each one gets an icon in the left rail.

[Download the latest release](https://github.com/bhumit070/tableplus_rs/releases/latest){ .md-button .md-button--primary }
[Source on GitHub](https://github.com/bhumit070/tableplus_rs){ .md-button }

## Supported databases

| Database | Browse & edit | Query editor |
| --- | --- | --- |
| PostgreSQL | ✓ | SQL |
| MySQL / MariaDB | ✓ | SQL |
| SQLite | ✓ | SQL |
| Redis | keys, types, TTLs, values | one command per line |
| MongoDB | collections as tables | mongosh-style: `db.users.find({age: {$gt: 30}})` |

## What's in these docs

<div class="grid cards" markdown>

- **[Getting started](getting-started.md)**: install Tabula, add a
  connection, import from TablePlus.
- **[Features](features.md)** and **[keyboard shortcuts](shortcuts.md)**:
  what the app can do and how to reach it.
- **[Architecture](architecture/overview.md)**: how the app is put
  together: the UI, the database layer, threading, storage and safety.
- **[Source files](modules.md)** and **[dependencies](dependencies.md)**:
  every module and every crate, and why it's there.
- **[Development](development.md)** and **[releases](releases.md)**:
  building, testing, and how CI publishes releases and these docs.

</div>
