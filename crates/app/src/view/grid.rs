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
use iced::widget::{column, container, row, scrollable, text};
use iced::{Element, Length};

const COLUMN_WIDTH: f32 = 160.0;

pub fn view(result: &QueryResult) -> Element<'_, Message> {
    if result.columns.is_empty() {
        let affected = result.rows_affected.unwrap_or(0);
        return container(text(format!("Query OK, {affected} row(s) affected.")).size(14)).padding(10).into();
    }

    let header = row(result
        .columns
        .iter()
        .map(|c| {
            container(text(c.name.clone()).size(13))
                .width(Length::Fixed(COLUMN_WIDTH))
                .padding(6)
                .style(container::rounded_box)
                .into()
        })
        .collect::<Vec<Element<'_, Message>>>());

    let mut body = column![].push(header);

    for data_row in &result.rows {
        let cells = row(data_row
            .iter()
            .map(|value| {
                let rendered = if value.is_null() { "NULL".to_string() } else { value.display() };
                container(text(rendered).size(13)).width(Length::Fixed(COLUMN_WIDTH)).padding(6).into()
            })
            .collect::<Vec<Element<'_, Message>>>());
        body = body.push(cells);
    }

    scrollable(body).width(Length::Fill).height(Length::Fill).into()
}
