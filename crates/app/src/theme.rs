//! The app's visual identity: one custom palette, applied everywhere
//! through iced's `Theme::custom`, plus a few named colors that fall
//! outside what a `Palette` covers (per-driver badges, muted/secondary
//! text, surface steps for cards).
//!
//! Using `Theme::custom` instead of a stock preset (`Theme::Dark`) means
//! every default widget style (buttons, containers, inputs, scrollbars)
//! derives from *this* palette automatically via iced's `Extended`
//! palette generation — one change here reshapes the whole app's look.
//!
//! These are plain constants, not looked up via `theme.extended_palette()`,
//! because `view()` functions only ever receive app state, never the
//! active `Theme` — only a widget's own `.style(|theme, status| ...)`
//! closure gets one. Keeping the palette as constants means every view
//! can reach for the same colors without needing a theme reference.

use db_core::connection::DriverKind;
use iced::theme::Palette;
use iced::Color;

pub const BACKGROUND: Color = color(0x1B, 0x1D, 0x22);
pub const TEXT: Color = color(0xE8, 0xEA, 0xED);
pub const PRIMARY: Color = color(0x4C, 0x8B, 0xF5);
pub const SUCCESS: Color = color(0x3D, 0xD6, 0x8C);
pub const WARNING: Color = color(0xF2, 0xB0, 0x3C);
pub const DANGER: Color = color(0xF0, 0x5F, 0x5F);

/// Secondary/muted text — labels, hints, detail captions.
pub const MUTED_TEXT: Color = color(0x8B, 0x90, 0x9C);

/// The sidebar/card surface color, one step lighter than the window
/// background so cards read as raised without needing a visible border.
pub const SURFACE: Color = color(0x23, 0x26, 0x2D);

/// A second, slightly lighter surface step — for rows/badges that sit
/// *on top of* a `SURFACE` card (e.g. the schema tree inside a
/// connection card) and need their own subtle separation.
pub const SURFACE_RAISED: Color = color(0x2A, 0x2E, 0x37);

pub const BORDER: Color = color(0x33, 0x37, 0x41);

pub fn theme() -> iced::Theme {
    iced::Theme::custom(
        "TablePlus-rs".to_string(),
        Palette { background: BACKGROUND, text: TEXT, primary: PRIMARY, success: SUCCESS, warning: WARNING, danger: DANGER },
    )
}

/// A small color badge per database driver, so the sidebar reads at a
/// glance the way a real multi-database client's does — Postgres blue,
/// MySQL orange, SQL Server's red — rather than every connection looking
/// identical save for its name.
pub fn driver_color(driver: DriverKind) -> Color {
    match driver {
        DriverKind::Postgres => color(0x4C, 0x8B, 0xF5),
        DriverKind::MySql => color(0xF2, 0x9A, 0x3C),
        DriverKind::MsSql => color(0xE0, 0x5A, 0x5A),
    }
}

const fn color(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}
