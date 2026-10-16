use crate::editor::highlighter::{self, SqlHighlighter};
use crate::message::Message;
use crate::model::App;
use db_core::export::ExportFormat;
use iced::advanced::text::highlighter::Format;
use iced::keyboard::key::Named;
use iced::keyboard::Key;
use iced::widget::{button, column, container, row, scrollable, text, text_editor, Space, Stack};
use iced::{Alignment, Element, Font, Length, Padding, Theme};

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = FONT_SIZE * 1.35;
const CHAR_WIDTH: f32 = FONT_SIZE * 0.62;
const EDITOR_PADDING: f32 = 8.0;

pub fn view(state: &App) -> Element<'_, Message> {
    let mut tab_bar = row![].spacing(4);
    for (i, tab) in state.tabs.iter().enumerate() {
        let is_active = i == state.active_tab;
        let label_button = button(text(tab.title(state)).size(13))
            .on_press(Message::SwitchTab(i))
            .style(if is_active { button::primary } else { button::text });
        tab_bar = tab_bar.push(
            row![label_button, button(text("x").size(11)).style(button::text).on_press(Message::CloseTab(i))]
                .spacing(2)
                .align_y(Alignment::Center),
        );
    }
    tab_bar = tab_bar.push(
        button(text("+").size(14))
            .style(button::text)
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

    let editor_area: Element<'_, Message> = if has_suggestions {
        let cursor = tab.editor.cursor().position;
        let top = EDITOR_PADDING + (cursor.line as f32 + 1.0) * LINE_HEIGHT;
        let left = EDITOR_PADDING + cursor.column as f32 * CHAR_WIDTH;

        let overlay = container(suggestion_dropdown(active_tab, tab))
            .padding(Padding { top, left, ..Padding::ZERO })
            .width(Length::Fill)
            .height(Length::Fill);

        Stack::with_children(vec![editor.into(), overlay.into()]).into()
    } else {
        editor.into()
    };

    let toolbar = row![
        button(text(if tab.running { "Running…" } else { "▶ Run" }).size(13))
            .on_press_maybe((!tab.running).then_some(Message::RunQuery(active_tab))),
        button(text("Cancel").size(13))
            .style(button::text)
            .on_press_maybe(tab.running.then_some(Message::CancelQuery(active_tab))),
        Space::new().width(Length::Fill),
        button(text("CSV").size(12)).style(button::text).on_press(Message::ExportResultRequested(active_tab, ExportFormat::Csv)),
        button(text("JSON").size(12)).style(button::text).on_press(Message::ExportResultRequested(active_tab, ExportFormat::Json)),
        button(text("SQL").size(12)).style(button::text).on_press(Message::ExportResultRequested(active_tab, ExportFormat::SqlInserts)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let mut results_area = column![toolbar].spacing(8).height(Length::FillPortion(2));
    if let Some(error) = &tab.error {
        results_area = results_area.push(container(text(error.clone()).size(13)).padding(8).style(container::rounded_box));
    }
    if let Some(result) = &tab.result {
        results_area = results_area.push(text(format!("{} row(s) in {:.2?}", result.row_count(), result.duration)).size(12));
        results_area = results_area.push(crate::view::grid::view(result));
    } else if tab.error.is_none() {
        results_area = results_area.push(container(text("Run a query to see results here.").size(13)).center(Length::Fill));
    }

    column![
        tab_bar,
        container(editor_area).height(Length::FillPortion(3)),
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
        .style(container::bordered_box)
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
