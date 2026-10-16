use crate::editor::highlighter::{self, SqlHighlighter};
use crate::message::Message;
use crate::model::App;
use db_core::export::ExportFormat;
use iced::advanced::text::highlighter::Format;
use iced::widget::{button, column, container, row, text, text_editor, Space};
use iced::{Element, Font, Length, Theme};

pub fn view(state: &App) -> Element<'_, Message> {
    let mut tab_bar = row![].spacing(4);
    for (i, tab) in state.tabs.iter().enumerate() {
        let label = tab.title(state);
        let is_active = i == state.active_tab;
        let label_button = button(text(label)).on_press(Message::SwitchTab(i)).style(if is_active {
            button::primary
        } else {
            button::secondary
        });
        tab_bar = tab_bar.push(row![label_button, button("x").on_press(Message::CloseTab(i))].spacing(2));
    }
    tab_bar = tab_bar.push(button("+").on_press(Message::NewTab(
        state.tabs.get(state.active_tab).and_then(|t| t.connection_id),
    )));

    let Some(tab) = state.tabs.get(state.active_tab) else {
        return container(text("No tab open")).into();
    };

    let dialect = tab.connection_id.and_then(|cid| state.sessions.get(&cid)).map(|s| s.config.driver);
    let active_tab = state.active_tab;

    let editor = text_editor(&tab.editor)
        .id(tab.id.to_string())
        .placeholder("Write a SQL query…")
        .height(Length::FillPortion(2))
        .highlight_with::<SqlHighlighter>(highlighter::Settings { dialect }, format_for)
        .key_binding(crate::editor::autopair::key_binding)
        .on_action(move |action| Message::EditorAction(active_tab, action));

    let mut editor_area = column![editor].spacing(4);

    if !tab.suggestions.is_empty() {
        let mut suggestion_list = row![].spacing(4);
        for (i, suggestion) in tab.suggestions.iter().take(10).enumerate() {
            suggestion_list = suggestion_list.push(
                button(text(suggestion.label.clone()).size(12)).on_press(Message::AcceptSuggestion(active_tab, i)),
            );
        }
        suggestion_list = suggestion_list.push(button(text("x").size(12)).on_press(Message::DismissSuggestions(active_tab)));
        editor_area = editor_area.push(container(suggestion_list).padding(4));
    }

    let toolbar = row![
        button(if tab.running { "Running…" } else { "Run (⌘⏎)" }).on_press_maybe((!tab.running).then_some(Message::RunQuery(active_tab))),
        button("Cancel").on_press_maybe(tab.running.then_some(Message::CancelQuery(active_tab))),
        Space::new().width(Length::Fill),
        button("Export CSV").on_press(Message::ExportResultRequested(active_tab, ExportFormat::Csv)),
        button("Export JSON").on_press(Message::ExportResultRequested(active_tab, ExportFormat::Json)),
        button("Export SQL").on_press(Message::ExportResultRequested(active_tab, ExportFormat::SqlInserts)),
    ]
    .spacing(6);

    let mut results_area = column![toolbar].spacing(6);
    if let Some(error) = &tab.error {
        results_area = results_area.push(text(error.clone()).size(13));
    }
    if let Some(result) = &tab.result {
        results_area = results_area.push(
            text(format!("{} row(s) in {:.2?}", result.row_count(), result.duration)).size(12),
        );
        results_area = results_area.push(crate::view::grid::view(result));
    }

    column![tab_bar, editor_area, results_area.height(Length::FillPortion(3))].spacing(8).padding(10).into()
}

fn format_for(kind: &sql_intel::TokenKind, _theme: &Theme) -> Format<Font> {
    Format { color: Some(highlighter::color_for(kind)), font: None }
}
