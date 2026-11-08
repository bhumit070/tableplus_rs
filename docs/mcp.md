# MCP server (AI tools)

Tabula can run an [MCP](https://modelcontextprotocol.io) server so AI tools
such as Claude Code or Cursor can work with your saved connections: list
databases and tables, read structure and rows, run queries, change rows, and
see or stop server processes. It's what the UI does, offered as tools.

It's **off** until you turn it on in **Settings › MCP**.

## Turning it on

1. Open **Settings** (⌘,) › **MCP** and check **Run the MCP server**.
2. The status line shows where it runs, for example
   `Running at http://127.0.0.1:53906/mcp`.
3. Copy what your tool needs:
    - **Claude Code command**: paste it into a terminal:

        ```sh
        claude mcp add --transport http tabula http://127.0.0.1:53906/mcp \
          --header "Authorization: Bearer <token>"
        ```

    - **JSON config**: for tools configured with an `mcpServers` file
      (Cursor, Claude Desktop through a bridge, …):

        ```json
        {
          "mcpServers": {
            "tabula": {
              "type": "http",
              "url": "http://127.0.0.1:53906/mcp",
              "headers": { "Authorization": "Bearer <token>" }
            }
          }
        }
        ```

    - Or **URL** and **Token** separately.

The tool sends the token with every request. You copy it once; Tabula keeps
its own copy in the keychain to check requests (never in its JSON files).

The first start picks a free port, and later starts reuse it while it's
free, so the tool's config keeps working. If that port is taken, Tabula
picks another one and the status line shows it; update the tool's config.

**Regenerate token** replaces the token at once: every tool using the old
one is cut off until you give it the new one.

## Tools

| Tool | What it does |
| --- | --- |
| `list_connections` | Saved connections: name, id, engine, host, database, environment tag, safe mode. Never passwords. |
| `list_databases` | Databases on a connection (Redis: 0–15). |
| `list_objects` | Tables, views, collections, Redis key namespaces and stored functions. |
| `describe_object` | Columns (type, NULL, default, key, comment), indexes, foreign keys, tables referencing it, the `CREATE` statement; a function's definition. |
| `read_rows` | Rows with filters (`eq`, `in`, `between`, `contains`, …), sort and paging, like the data grid; MongoDB also takes a `find` document and `fields`. Up to 1,000 rows per call. |
| `run_query` | SQL (several statements), Redis commands (one per line) or MongoDB shell calls, like the query editor. |
| `change_rows` | Inserts, updates and deletes rows of one table in one transaction, like committing staged edits. |
| `list_processes` / `kill_process` | The process list; cancel a statement or end a session. |
| `redis_key` | A Redis key's type, TTL, size and first 500 elements. |

Each tool takes the connection by **name or id**, and an optional
`database` to use instead of the connection's default.

## Safety

- **This computer only.** The server listens on `127.0.0.1`, checks the
  `Host` header (against DNS rebinding) and refuses any request without the
  token.
- **Safe mode applies.** Every write follows the connection's safe mode,
  exactly as in the UI:
    - *Read-only* refuses writes (the tool gets the reason).
    - *Confirm writes* / *Confirm every query* show the usual confirmation
      in the Tabula window, with a note that an AI tool asked. The tool
      waits up to five minutes; closing the dialog answers "no".
    - *Off* (and *Automatic* away from Production) runs it.
- **No passwords leave Tabula.** The server connects with the saved
  passwords from the keychain itself; tools only see names.
- **Your data goes to the tool.** Anything a tool reads may be sent to its
  model provider. Use read-only or confirm modes for connections you don't
  want an AI tool to change, and keep the server off when you don't use it.

If **Save passwords** is off, the token can't be kept in the keychain: it
lives for this run only, and tools need the new one after Tabula restarts
(Settings says so).
