//! Small, reusable styled pieces shared across the sidebar, query tab,
//! and dialogs, so the app reads as one designed surface instead of
//! every view improvising its own card/badge look.

use crate::message::Message;
use db_core::connection::DriverKind;
use iced::widget::{container, text};
use iced::{Border, Color, Element, Length};

/// A small circular badge carrying the driver's initial, colored per
/// backend (Postgres blue / MySQL orange / SQL Server red) — the same
/// glanceable cue a real multi-database client gives you instead of
/// every connection row looking identical save for its name.
pub fn driver_badge<'a>(driver: DriverKind) -> Element<'a, Message> {
    let letter = match driver {
        DriverKind::Postgres => "P",
        DriverKind::MySql => "M",
        DriverKind::MsSql => "S",
    };

    container(text(letter).size(11).color(Color::WHITE))
        .center(Length::Fixed(20.0))
        .style(move |_theme| container::Style {
            background: Some(crate::theme::driver_color(driver).into()),
            border: Border { radius: 10.0.into(), ..Border::default() },
            ..container::Style::default()
        })
        .into()
}

/// A small status dot: filled green when connected, a half-circle while
/// connecting, a hollow ring when idle.
pub fn status_dot<'a>(connecting: bool, connected: bool) -> Element<'a, Message> {
    let (glyph, color) = if connecting {
        ("◐", crate::theme::WARNING)
    } else if connected {
        ("●", crate::theme::SUCCESS)
    } else {
        ("○", crate::theme::MUTED_TEXT)
    };
    text(glyph).size(11).color(color).into()
}

/// The resting card style used for the connection list and other
/// surfaces that sit directly on the window background.
pub fn card(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(crate::theme::SURFACE.into()),
        border: Border { color: crate::theme::BORDER, width: 1.0, radius: 10.0.into() },
        ..container::Style::default()
    }
}

/// Like [`card`], but with an accent-colored border — used to show the
/// currently-expanded/active connection without relying on a background
/// color shift alone (which reads poorly for colorblind users and is
/// easy to miss at a glance).
pub fn card_active(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(crate::theme::SURFACE_RAISED.into()),
        border: Border { color: crate::theme::PRIMARY, width: 1.0, radius: 10.0.into() },
        ..container::Style::default()
    }
}

/// A banner for inline error text — a colored left accent and a tinted
/// background instead of plain red text floating in the layout.
pub fn error_banner(_theme: &iced::Theme) -> container::Style {
    let danger = crate::theme::DANGER;
    container::Style {
        background: Some(Color { a: 0.12, ..danger }.into()),
        border: Border { color: danger, width: 1.0, radius: 6.0.into() },
        text_color: Some(danger),
        ..container::Style::default()
    }
}

/// A muted section header, e.g. "CONNECTIONS" above the sidebar list.
pub fn section_header(label: &str) -> Element<'_, Message> {
    text(label.to_uppercase()).size(12).color(crate::theme::MUTED_TEXT).into()
}
