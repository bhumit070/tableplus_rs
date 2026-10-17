use super::components;
use crate::editor::highlighter::{self, SqlHighlighter};
use crate::message::Message;
use crate::model::App;
use db_core::export::ExportFormat;
use iced::advanced::text::highlighter::Format;
use iced::keyboard::key::Named;
use iced::keyboard::Key;
use iced::widget::{button, column, container, pick_list, row, scrollable, text, text_editor, Space, Stack};
use iced::{Alignment, Border, Color, Element, Font, Length, Padding, Theme};
use uuid::Uuid;

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = FONT_SIZE * 1.35;
const CHAR_WIDTH: f32 = FONT_SIZE * 0.62;
const EDITOR_PADDING: f32 = 8.0;

pub fn view(state: &App) -> Element<'_, Message> {
    let mut tab_bar = row![].spacing(6);
    for (i, tab) in state.tabs.iter().enumerate() {
        let is_active = i == state.active_tab;
        let chip = row![
            button(text(tab.title(state)).size(13)).on_press(Message::SwitchTab(i)).style(button::text).padding(0),
            button(text("×").size(13)).style(button::text).padding(0).on_press(Message::CloseTab(i)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        tab_bar = tab_bar.push(
            container(chip)
                .padding([6, 12])
                .style(move |_theme| {
                    if is_active {
                        container::Style {
                            background: Some(crate::theme::SURFACE_RAISED.into()),
                            border: Border { color: crate::theme::PRIMARY, width: 1.0, radius: 6.0.into() },
                            ..container::Style::default()
                        }
                    } else {
                        container::Style::default()
                    }
                }),
        );
    }
    tab_bar = tab_bar.push(
        button(text("+").size(15))
            .style(button::text)
            .padding([6, 10])
            .on_press(Message::NewTab(state.tabs.get(state.active_tab).and_then(|t| t.connection_id))),
    );

    let Some(tab) = state.tabs.get(state.active_tab) else {
        return container(text("No tab open")).center(Length::Fill).into();
    };

    let dialect = tab.connection_id.and_then(|cid| state.sessions.get(&cid)).map(|s| s.config.driver);
    let active_tab = state.active_tab;
    let has_suggestions = !tab.suggestions.is_empty();
    let selected_suggestion = tab.selected_suggestion;

    let editor = text_editor(&tab.editor)
        .id(tab.id.to_string())
        .font(Font::MONOSPACE)
        .size(FONT_SIZE)
        .padding(EDITOR_PADDING)
        .placeholder("Write a SQL query…")
        .height(Length::Fill)
        .highlight_with::<SqlHighlighter>(highlighter::Settings { dialect }, format_for)
        .key_binding(move |key_press| {
            if has_suggestions {
                match key_press.modified_key.as_ref() {
                    Key::Named(Named::ArrowDown) => {
                        return Some(text_editor::Binding::Custom(Message::SelectNextSuggestion(active_tab)))
                    }
                    Key::Named(Named::ArrowUp) => {
                        return Some(text_editor::Binding::Custom(Message::SelectPrevSuggestion(active_tab)))
                    }
                    Key::Named(Named::Tab) | Key::Named(Named::Enter) => {
                        return Some(text_editor::Binding::Custom(Message::AcceptSuggestion(
                            active_tab,
                            selected_suggestion,
                        )))
                    }
                    Key::Named(Named::Escape) => {
                        return Some(text_editor::Binding::Custom(Message::DismissSuggestions(active_tab)))
                    }
                    _ => {}
                }
            }
            crate::editor::autopair::key_binding(key_press)
        })
        .on_action(move |action| Message::EditorAction(active_tab, action));

    // Important: this `Stack` must *always* wrap the editor, never only
    // conditionally when suggestions are showing. iced's widget tree
    // diffing keys a widget's internal state (focus, cursor, selection)
    // to its position/shape in the tree; switching between "editor
    // alone" and "Stack[editor, overlay]" from one frame to the next
    // made iced treat it as a different widget and silently reset that
    // state — which is why focus used to jump away whenever a
    // suggestion appeared.
    let overlay: Element<'_, Message> = if has_suggestions {
        let cursor = tab.editor.cursor().position;
        let top = EDITOR_PADDING + (cursor.line as f32 + 1.0) * LINE_HEIGHT;
        let left = EDITOR_PADDING + cursor.column as f32 * CHAR_WIDTH;

        container(suggestion_dropdown(active_tab, tab))
            .padding(Padding { top, left, ..Padding::ZERO })
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else {
        Space::new().width(Length::Shrink).height(Length::Shrink).into()
    };
    let editor_area: Element<'_, Message> = Stack::with_children(vec![editor.into(), overlay]).into();

    let connection_options: Vec<ConnectionOption> = state
        .sessions
        .values()
        .filter(|s| s.adapter.is_some())
        .map(|s| ConnectionOption { id: s.config.id, name: s.config.name.clone() })
        .collect();
    let selected_connection =
        tab.connection_id.and_then(|cid| connection_options.iter().find(|o| o.id == cid).cloned());

    let toolbar = row![
        button(text(if tab.running { "Running…" } else { "▶  Run" }).size(13))
            .padding([6, 14])
            .on_press_maybe((!tab.running).then_some(Message::RunQuery(active_tab))),
        button(text("Cancel").size(13))
            .style(button::text)
            .on_press_maybe(tab.running.then_some(Message::CancelQuery(active_tab))),
        pick_list(connection_options, selected_connection, move |option| {
            Message::SetTabConnection(active_tab, option.id)
        })
        .placeholder("Not connected")
        .text_size(12),
        Space::new().width(Length::Fill),
        text("Export:").size(11).color(crate::theme::MUTED_TEXT),
        button(text("CSV").size(12)).style(button::text).on_press(Message::ExportResultRequested(active_tab, ExportFormat::Csv)),
        button(text("JSON").size(12)).style(button::text).on_press(Message::ExportResultRequested(active_tab, ExportFormat::Json)),
        button(text("SQL").size(12)).style(button::text).on_press(Message::ExportResultRequested(active_tab, ExportFormat::SqlInserts)),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let mut results_area = column![toolbar].spacing(10).height(Length::FillPortion(2));
    if let Some(error) = &tab.error {
        results_area = results_area
            .push(container(text(error.clone()).size(13)).padding(10).width(Length::Fill).style(components::error_banner));
    }
    if let Some(result) = &tab.result {
        results_area = results_area.push(
            text(format!("{} row(s) in {:.2?}", result.row_count(), result.duration))
                .size(11)
                .color(crate::theme::MUTED_TEXT),
        );
        results_area = results_area.push(container(crate::view::grid::view(result)).style(components::card));
    } else if tab.error.is_none() {
        results_area = results_area.push(
            container(text("Run a query to see results here.").size(13).color(crate::theme::MUTED_TEXT))
                .style(components::card)
                .center(Length::Fill),
        );
    }

    column![
        tab_bar,
        container(editor_area).height(Length::FillPortion(3)).style(components::card),
        results_area,
    ]
    .spacing(10)
    .padding(14)
    .height(Length::Fill)
    .width(Length::Fill)
    .into()
}

fn suggestion_dropdown<'a>(tab_index: usize, tab: &'a crate::model::Tab) -> Element<'a, Message> {
    let mut list = column![].spacing(0);
    for (i, suggestion) in tab.suggestions.iter().take(12).enumerate() {
        let is_selected = i == tab.selected_suggestion;
        let row_content = row![
            text(kind_glyph(suggestion.kind)).size(12),
            text(suggestion.label.clone()).size(13),
            Space::new().width(Length::Fill),
            text(suggestion.detail.clone().unwrap_or_default()).size(11),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        list = list.push(
            button(row_content)
                .width(Length::Fill)
                .padding([4, 8])
                .style(if is_selected { button::primary } else { button::text })
                .on_press(Message::AcceptSuggestion(tab_index, i)),
        );
    }

    container(scrollable(list).height(Length::Shrink))
        .width(Length::Fixed(320.0))
        .padding(2)
        .style(|_theme| container::Style {
            background: Some(crate::theme::SURFACE_RAISED.into()),
            border: Border { color: crate::theme::BORDER, width: 1.0, radius: 8.0.into() },
            shadow: iced::Shadow { color: Color::BLACK, offset: iced::Vector::new(0.0, 4.0), blur_radius: 16.0 },
            ..container::Style::default()
        })
        .into()
}

fn kind_glyph(kind: sql_intel::SuggestionKind) -> &'static str {
    match kind {
        sql_intel::SuggestionKind::Keyword => "𝐊",
        sql_intel::SuggestionKind::Schema => "𝐒",
        sql_intel::SuggestionKind::Table => "▤",
        sql_intel::SuggestionKind::Column => "▥",
        sql_intel::SuggestionKind::Function => "ƒ",
    }
}

fn format_for(kind: &sql_intel::TokenKind, _theme: &Theme) -> Format<Font> {
    Format { color: Some(highlighter::color_for(kind)), font: None }
}

#[derive(Clone, PartialEq)]
struct ConnectionOption {
    id: Uuid,
    name: String,
}

impl std::fmt::Display for ConnectionOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}
