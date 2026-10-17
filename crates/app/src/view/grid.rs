//! A plain, un-virtualized results table.
//!
//! Good enough for the hundreds-of-rows results a human is actually
//! looking at in an editor; it will get slow on a million-row dump. A
//! virtualized grid (only building widgets for visible rows) is the
//! natural follow-up — see DEVELOPMENT.md's roadmap — but needs either
//! `iced::widget::canvas` or a scroll-position-aware custom widget, both
//! sizable efforts on their own.

use crate::message::Message;
use db_core::result::QueryResult;
use iced::widget::scrollable::{Direction, Scrollbar};
use iced::widget::{column, container, row, scrollable, text};
use iced::{Border, Color, Element, Length};

const COLUMN_WIDTH: f32 = 160.0;

pub fn view(result: &QueryResult) -> Element<'_, Message> {
    if result.columns.is_empty() {
        let affected = result.rows_affected.unwrap_or(0);
        return container(text(format!("Query OK, {affected} row(s) affected.")).size(14).color(crate::theme::MUTED_TEXT))
            .padding(12)
            .into();
    }

    let header = row(result
        .columns
        .iter()
        .map(|c| {
            container(text(c.name.clone()).size(12).color(crate::theme::MUTED_TEXT))
                .width(Length::Fixed(COLUMN_WIDTH))
                .padding([8, 10])
                .into()
        })
        .collect::<Vec<Element<'_, Message>>>());

    let header_bar = container(header).style(|_theme| container::Style {
        background: Some(crate::theme::SURFACE_RAISED.into()),
        border: Border { color: crate::theme::BORDER, width: 0.0, radius: 0.0.into() },
        ..container::Style::default()
    });

    // The header lives *inside* the scrollable, alongside the rows, not
    // pinned above it — pinning it would mean it stops lining up with
    // the data columns the moment you scroll horizontally, which is
    // worse than not pinning it at all. A sticky header needs a
    // genuinely different widget (independent horizontal scroll synced
    // between a fixed header and the body), which is future work.
    let mut body = column![header_bar];
    for (i, data_row) in result.rows.iter().enumerate() {
        let cells = row(data_row
            .iter()
            .map(|value| {
                let rendered = if value.is_null() { "NULL".to_string() } else { value.display() };
                let muted = value.is_null();
                container(text(rendered).size(13).color(if muted { crate::theme::MUTED_TEXT } else { crate::theme::TEXT }))
                    .width(Length::Fixed(COLUMN_WIDTH))
                    .padding([6, 10])
                    .into()
            })
            .collect::<Vec<Element<'_, Message>>>());

        // A faint zebra stripe makes it much easier to track a row
        // across many columns than a flat background would.
        let striped = i % 2 == 1;
        body = body.push(container(cells).style(move |_theme| container::Style {
            background: striped.then_some(Color { a: 0.03, ..Color::WHITE }.into()),
            ..container::Style::default()
        }));
    }

    // `scrollable`'s default direction is vertical-only — a result with
    // more columns than fit the window (a plain `SELECT *` on any table
    // with a dozen-plus columns) would just get clipped on the right with
    // no way to reach the rest. `Both` gives it a horizontal scrollbar too.
    scrollable(body)
        .direction(Direction::Both { horizontal: Scrollbar::default(), vertical: Scrollbar::default() })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
