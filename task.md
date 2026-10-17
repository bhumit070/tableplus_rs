# Task tracker — visual design pass

Started: 2026-10-17 00:05

The functional bugs (keychain, editor focus, type decoding, grid scrolling) are fixed and merged. This pass is purely about making the app look and feel like a considered product instead of default-widget-soup. Each task flips to `[x] done (timestamp)` only once built, linted, and smoke-run.

## Plan

- [x] done (2026-10-17 09:10) — Custom color palette/theme (`crates/app/src/theme.rs`): deep charcoal background (`#1B1D22`), blue accent (`#4C8BF5`), proper success/warning/danger colors, via `iced::Theme::custom` so every default button/container/input style derives from it automatically instead of the stock `Theme::Dark`.
- [x] done (2026-10-17 09:20) — Shared styled components (`crates/app/src/view/components.rs`): per-driver color badge (Postgres blue / MySQL orange / SQL Server red), status dot, card/card-active container styles, an error banner style, a muted section-header helper — so the sidebar, query tab, and dialog all reuse the same visual language instead of each inventing its own.
- [x] done (2026-10-17 09:25) — Sidebar: connection rows now show a colored driver badge + name + driver label + status dot instead of a plain unicode glyph; the expanded/active connection gets an accent-colored card border; a real "CONNECTIONS" section header; errors render in a proper tinted banner instead of plain text.
- [x] done (2026-10-17 09:35) — Query tab: active tab is now a bordered "chip" instead of a plain button; toolbar grouped (Run/Cancel/connection picker, then an "Export:" label + CSV/JSON/SQL); the editor and results both sit in a bordered card; error text uses the shared error-banner style; empty-result state is muted instead of full-brightness text.
- [x] done (2026-10-17 09:40) — Results grid: styled header row (muted text on a raised surface), zebra-striped rows (3% white overlay on alternating rows), `NULL` cells rendered in muted gray instead of looking like real data.
- [x] done (2026-10-17 09:50) — Connection dialog: real card styling (border + drop shadow + radius), muted field labels, Host/Port side-by-side instead of stacked, colored test-connection result (green on success, red on failure).
- [x] done (2026-10-17 09:55) — Verification: `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (29 tests) all clean; smoke-ran the binary (starts without panicking). I still can't screenshot/drive the native window myself in this session, so the actual visual result needs your eyes — please look and tell me what still misses the mark.

## Known still-rough edges (not attempted in this pass)

- No light theme / theme switching.
- Table icons (▤/◫/◧) and suggestion-kind glyphs (𝐊/▥/ƒ) are still Unicode symbols, not real icons — a proper icon font or SVG set would look more polished but is a larger, separate effort (font embedding + icon sourcing).
- No sticky/pinned header on horizontal scroll in the results grid — the header scrolls with the data (noted in `grid.rs`); a genuinely sticky header needs a different widget arrangement.
