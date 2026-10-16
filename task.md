# Task tracker — UI/UX and functional fixes

Started: 2026-10-16 23:00

Each task starts `[ ]` pending. I only flip it to `[x] done (timestamp)` once I've actually made the change and re-verified it (build/test or a manual check) — not before.

## Diagnosis

- [x] done (2026-10-16 23:10) — Root-cause why the SQL editor appears broken/not working: `query_tab::view`'s outer `column![...]` (and the `editor_area` wrapper column inside it) had no explicit `.height(...)`, defaulting to `Length::Shrink`. The editor and results grid were set to `Length::FillPortion(n)`, but `FillPortion` only does anything when the *parent* is itself sized `Fill`/`FillPortion` — under a `Shrink` parent it collapses toward zero height. Confirmed by reading `iced_widget`'s row/column layout code: a `Fill` parent does not make its children `Fill` automatically, each child only stretches if it individually requests `Fill`/`FillPortion`.
- [x] done (2026-10-16 23:10) — Root-cause why no tables show up in the sidebar: not a broken state/fetch bug (connect → fetch-catalog → render chain was logically correct and auto-expands on connect) — it's a *visibility/crowding* bug. The old connection row crammed a name-toggle button plus 4 action buttons into a fixed 280px-wide column, which pushed/wrapped content so badly the schema tree underneath was effectively unreadable even once populated. No catalog-fetch logic bug found.

## Fixes

- [x] done (2026-10-16 23:25) — Fixed the layout: `query_tab::view`'s root column and the editor/results containers now carry explicit `Length::Fill`/`FillPortion`, so the editor and results grid actually get real estate and resize with the window. Verified via `cargo build` + reading the generated layout chain top to bottom.
- [x] done (2026-10-16 23:45) — Redesigned the sidebar: each connection is now a compact one-line row (status dot + name + driver label) that connects/expands on click; the Query/Edit/Disconnect/Delete actions only appear, as small low-emphasis buttons, when that connection is expanded. Cards have breathing room (`rounded_box` style, consistent padding) instead of one dense wrapped row.
- [x] done (2026-10-17 00:05) — Rebuilt autocomplete as a floating dropdown: an `iced::widget::Stack` overlays a bordered suggestion card on top of the editor, offset from the editor's top-left corner by an estimate of the cursor's pixel position (line × line-height, column × monospace char-width — the editor font is now explicitly `Font::MONOSPACE` so that estimate is accurate), instead of a row of small buttons pinned to the bottom of the whole pane.
- [x] done (2026-10-17 00:05) — Wired real keyboard navigation: Up/Down now move `tab.selected_suggestion` (new `SelectNextSuggestion`/`SelectPrevSuggestion` messages), Tab/Enter accept the highlighted suggestion, Escape dismisses the dropdown — all via a `key_binding` closure that only intercepts those keys while suggestions are showing, falling back to normal typing/auto-pair behavior otherwise.
- [x] done (2026-10-17 00:15) — Spacing/typography pass: consistent small-caps-style section header in the sidebar, status dot instead of a text suffix, toolbar buttons given consistent sizing (13px primary actions, 12px secondary), results area shows an explicit empty state ("Run a query to see results here.") instead of blank space.
- [x] done (2026-10-17 00:30) — Resizing: every pane in the layout now resolves to `Length::Fill`/`FillPortion` end-to-end (sidebar, tab bar, editor, results), so resizing the window redistributes space instead of leaving fixed-size gaps. I can't drag-resize the window myself in this session (no screen/GUI automation available to me here) — flagging this so you know it's reasoned from the layout code + a headless build/run check, not an eyeballed screenshot. Please resize the window and tell me if anything still looks wrong.

## Verification

- [x] done (2026-10-17 00:32) — `cargo build --workspace`, `cargo test --workspace` (28 tests), `cargo clippy --workspace --all-targets -- -D warnings` all clean.
- [x] done (2026-10-17 00:33) — Smoke-ran the binary (`./target/debug/tableplus-rs`) in the background for several seconds: starts cleanly, no panics/errors in stdout/stderr, stays alive.
- [ ] Manual smoke test with a real connection (connect → see tables → type a query → see autocomplete → run → see results) — this needs your eyes on the actual window; I don't have a way to drive or screenshot a native GUI window in this session. **Please try it and tell me what you see** — if the sidebar/editor/autocomplete still don't look right, send me specifics (ideally a screenshot) and I'll keep iterating.
