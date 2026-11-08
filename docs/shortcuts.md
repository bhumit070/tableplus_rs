# Keyboard shortcuts

On Windows and Linux, use **Ctrl** in place of **⌘**. Shortcuts work
wherever focus is: each acts on the current connection and tab. Every
command is also in the menu bar.

## Connections

| Shortcut | Action |
| --- | --- |
| ⌘N or ⌘⇧K | Open a connection: switch to an open one, open a saved one, or add a new one |
| ⌘1 … ⌘9 | Switch to the 1st … 9th open connection |
| ⌘0 | Connection list |
| / | On the connection list: search |
| ↵ | On the connection list: reopen last session (when offered) |
| ⌘K | Switch database |
| ⌘P | Go to a table, view, collection, column or saved query, or run a command |
| Tab / ⇧Tab | In ⌘P: next / previous scope (All, Tables, Columns, Saved queries, Commands…) |
| ⌘↵ | In ⌘P: open the highlighted table in a new tab, even if it's open already |
| ⌥↵ | In ⌘P: copy the highlighted name, qualified (`schema.table`, `table.column`) |
| ⌘⇧P | Process list |
| ⌘R | Refresh the table (or the sidebar) |

## Tabs

| Shortcut | Action |
| --- | --- |
| ⌘T | New query tab |
| ⌘W | Close tab |
| ⌘⇧] / ⌘⇧[ | Next / previous tab |

## Query editor

| Shortcut | Action |
| --- | --- |
| ⌘↵ | Run the statement at the cursor, or the selection |
| ⌘⇧↵ | Run everything in the editor (one result tab per statement) |
| ⌘. | Stop the running query |
| ⌘E | Explain the query |
| ⌘⇧F | Format SQL |
| ⌘Y | Query history |
| ⌘⇧S / ⌘⇧O | Save query / open a saved query |
| Ctrl+Space | Show autocomplete |

## Data

| Shortcut | Action |
| --- | --- |
| ⌘S | Commit staged changes |
| ⌘⇧⌫ | Discard staged changes |
| ⌘I | Add a row |
| ⌘⌫ | Delete the selected row (outside text fields) |
| ⌘F | Show / hide the filter editor (outside text fields); filters stay applied as chips |
| ⌘C | Copy the selected cell |
| ⌘⇧E | Export the table or result |
| ⌘⇧I | Row detail panel |

## Cell editing

| Shortcut | Action |
| --- | --- |
| Enter or typing | Edit the selected cell (typing replaces the value) |
| Enter | Save the edit |
| Tab / ⇧Tab | Save and edit the next / previous cell |
| Esc | Cancel the edit |
| ↑ / ↓ (⇧ for 10) | While editing a number: step it |
| ⌥⌫ | Set the selected cell to NULL |
| ⌥↵ | Open the bigger editor (long text, JSON) |
| Space | Flip a boolean cell |
| ⌘↵ | In the bigger editor: stage the change |

In an enum or boolean dropdown: ↑ / ↓ move, Enter or a click picks, 1–9
pick by position, and typing a letter jumps to the next value starting with
it.

## App

| Shortcut | Action |
| --- | --- |
| ⌘, | Settings |
| ⌘= / ⌘− | Zoom in / out |
| ⌘⇧L | Toggle light / dark theme |
| ⌘Q | Quit |

Shortcuts are defined in `key_bindings()` in `src/ui/mod.rs`; see
[User interface](architecture/ui.md#actions-and-shortcuts) for how they're
routed.
