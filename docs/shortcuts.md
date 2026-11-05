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
| ⌘K | Switch database |
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
| ⌘↵ | Run the selection, or the whole editor |
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
| ⌘F | Toggle the filter bar (outside text fields) |
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
