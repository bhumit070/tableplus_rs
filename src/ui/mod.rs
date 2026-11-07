pub mod app;
pub mod cell_edit;
pub mod cell_value;
pub mod completion;
pub mod confirm;
pub mod conn_form;
pub mod copy;
pub mod grid;
pub mod home;
pub mod picker;
pub mod processes;
pub mod query_tab;
pub mod row_panel;
pub mod session;
pub mod settings_view;
pub mod table_editor;
pub mod table_tab;
pub mod theme;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Hsla, KeyBinding};
use tokio::sync::RwLock;

use crate::db::{Connection, ConnectionConfig};

/// Web-style cursors on disabled controls: gpui-component keeps the arrow,
/// we show "not allowed" (and the hand, set where each control is built,
/// while enabled).
pub trait DisabledWhen: gpui_component::Disableable + gpui::Styled + Sized {
    fn disabled_when(self, disabled: bool) -> Self {
        let this = self.disabled(disabled);
        if disabled {
            this.cursor_not_allowed()
        } else {
            this
        }
    }
}

impl<T: gpui_component::Disableable + gpui::Styled> DisabledWhen for T {}

gpui::actions!(
    tabula,
    [
        RunQuery,
        CancelQuery,
        QueryHistory,
        ExportData,
        ToggleRowDetail,
        FormatSql,
        SaveQuery,
        SavedQueries,
        ShowProcesses,
        BackupDatabase,
        RestoreDatabase,
        ExplainQuery,
        NewDatabase,
        DropDatabase,
        EditCell,
        CancelCellEdit,
        NextCell,
        PrevCell,
        ChoiceUp,
        ChoiceDown,
        ChoicePick,
        SwitchDatabase,
        GoTo,
        Commit,
        Discard,
        Refresh,
        NewQuery,
        CloseTab,
        NextTab,
        PrevTab,
        ToggleTheme,
        ToggleFilter,
        DeleteRows,
        AddRow,
        CopyCell,
        SetNull,
        OpenCellEditor,
        CellUp,
        CellDown,
        CellUpMore,
        CellDownMore,
        GoHome,
        OpenConnection,
        Connection1,
        Connection2,
        Connection3,
        Connection4,
        Connection5,
        Connection6,
        Connection7,
        Connection8,
        Connection9,
        OpenSettings,
        ZoomIn,
        ZoomOut,
        Quit,
    ]
);

/// One open connection, shared by the session and all of its tabs.
pub struct Shared {
    pub cfg: ConnectionConfig,
    pub conn: Arc<RwLock<Connection>>,
    /// Names for autocomplete; refreshed whenever the sidebar reloads.
    pub schema: Rc<RefCell<completion::Schema>>,
}

/// Applies the safe mode: returns true when the caller may go ahead now;
/// otherwise asks for confirmation (showing `detail`, then calling `go`)
/// or explains why it's blocked, and returns false. (`go` runs later, so
/// it may update the caller's entity.)
pub fn guarded(
    gate: crate::db::safety::Gate,
    cfg: &ConnectionConfig,
    what: &str,
    detail: String,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
    go: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> bool {
    use crate::db::safety::Gate;
    use gpui_component::WindowExt as _;
    match gate {
        Gate::Run => return true,
        Gate::Block(why) => {
            window.push_notification(gpui_component::notification::Notification::warning(why), cx)
        }
        // Snoozed for a while ("Confirm writes" only).
        Gate::Confirm if confirm::snoozed(cfg, cx) => return true,
        Gate::Confirm => {
            confirm::ConfirmWrite::open(cfg, what, detail, window, cx, std::rc::Rc::new(go))
        }
    }
    false
}

/// A small dialog asking for one line of text. `on_ok` gets the trimmed
/// text and returns false to keep the dialog open (e.g. invalid input).
pub fn prompt_text(
    title: &str,
    description: &str,
    initial: &str,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
    on_ok: impl Fn(String, &mut gpui::Window, &mut gpui::App) -> bool + 'static,
) {
    use gpui::{AppContext as _, ParentElement as _, Styled as _};
    use gpui_component::WindowExt as _;
    use gpui_component::input::{Input, InputState};
    let initial = initial.to_string();
    let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
    let (title, description) = (title.to_string(), description.to_string());
    let on_ok = std::rc::Rc::new(on_ok);
    let first = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        use gpui::prelude::FluentBuilder as _;
        use gpui_component::Sizable as _;
        use gpui_component::button::{Button, ButtonVariants as _};
        if first.replace(false) {
            input.update(cx, |i, cx| {
                i.focus(window, cx);
                i.select_all(window, cx);
            });
        }
        let submit = {
            let (input, on_ok) = (input.clone(), on_ok.clone());
            move |window: &mut gpui::Window, cx: &mut gpui::App| -> bool {
                let v = input.read(cx).value().trim().to_string();
                !v.is_empty() && on_ok(v, window, cx)
            }
        };
        let submit = std::rc::Rc::new(submit);
        let (s1, s2) = (submit.clone(), submit);
        dialog
            .title(title.clone())
            .w(gpui::px(420.))
            .child(
                gpui_component::v_flex()
                    .gap_2()
                    .when(!description.is_empty(), |d| d.child(description.clone()))
                    .child(Input::new(&input)),
            )
            // Enter in the field.
            .on_ok(move |_, window, cx| s1(window, cx))
            .footer(
                gpui_component::dialog::DialogFooter::new()
                    .child(
                        Button::new("prompt-cancel")
                            .cursor_pointer()
                            .label("Cancel")
                            .small()
                            .on_click(|_, w, cx| w.close_dialog(cx)),
                    )
                    .child(
                        Button::new("prompt-ok")
                            .cursor_pointer()
                            .label("OK")
                            .primary()
                            .small()
                            .on_click(move |_, window, cx| {
                                if s2(window, cx) {
                                    window.close_dialog(cx);
                                }
                            }),
                    ),
            )
    });
}

/// Items for a searchable dropdown. A plain `Vec` also works as a dropdown
/// list, but its search does nothing (GitHub #12); `SearchableVec` filters.
pub type SearchList = gpui_component::select::SearchableVec<String>;

pub fn search_list(items: Vec<String>) -> SearchList {
    gpui_component::select::SearchableVec::new(items)
}

/// The connection's environment colour (design.md, Environment tags).
pub fn tag_color(cfg: &ConnectionConfig, cx: &App) -> Hsla {
    theme::env_color(cfg.tag, cx)
}

/// `sec!` is ⌘ on macOS and Ctrl elsewhere. Bindings without a context
/// work wherever focus is; `app::register_actions` routes them to the
/// current connection and tab. Note gpui ranks context-free bindings above
/// every context, so keys that text fields use must say `!Input`.
/// Platform modifier: `sec!("k")` is "cmd-k" on macOS, "ctrl-k" elsewhere.
/// (gpui's own `secondary-` alias only works in keymap files.)
macro_rules! sec {
    ($key:literal) => {
        if cfg!(target_os = "macos") {
            concat!("cmd-", $key)
        } else {
            concat!("ctrl-", $key)
        }
    };
}

pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        // Beats the editor's own ⌘↵, which would insert a newline.
        KeyBinding::new(sec!("enter"), RunQuery, Some("QueryTab > Input")),
        KeyBinding::new(sec!("enter"), RunQuery, None),
        KeyBinding::new(sec!("."), CancelQuery, None),
        KeyBinding::new(sec!("y"), QueryHistory, None),
        KeyBinding::new(sec!("shift-e"), ExportData, None),
        KeyBinding::new(sec!("shift-i"), ToggleRowDetail, None),
        KeyBinding::new(sec!("shift-f"), FormatSql, None),
        KeyBinding::new(sec!("shift-s"), SaveQuery, None),
        KeyBinding::new(sec!("shift-o"), SavedQueries, None),
        KeyBinding::new(sec!("shift-p"), ShowProcesses, None),
        KeyBinding::new(sec!("e"), ExplainQuery, None),
        KeyBinding::new(sec!("k"), SwitchDatabase, None),
        KeyBinding::new(sec!("p"), GoTo, None),
        KeyBinding::new(sec!("s"), Commit, None),
        KeyBinding::new(sec!("shift-backspace"), Discard, None),
        KeyBinding::new(sec!("r"), Refresh, None),
        // Text fields use these keys (editor search, delete to line start).
        KeyBinding::new(sec!("f"), ToggleFilter, Some("!Input")),
        KeyBinding::new(sec!("backspace"), DeleteRows, Some("!Input")),
        KeyBinding::new(sec!("i"), AddRow, None),
        KeyBinding::new(sec!("t"), NewQuery, None),
        KeyBinding::new(sec!("w"), CloseTab, None),
        KeyBinding::new(sec!("shift-]"), NextTab, None),
        KeyBinding::new(sec!("shift-["), PrevTab, None),
        KeyBinding::new(sec!("0"), GoHome, None),
        KeyBinding::new(sec!("n"), OpenConnection, None),
        KeyBinding::new(sec!("shift-k"), OpenConnection, None),
        KeyBinding::new(sec!("1"), Connection1, None),
        KeyBinding::new(sec!("2"), Connection2, None),
        KeyBinding::new(sec!("3"), Connection3, None),
        KeyBinding::new(sec!("4"), Connection4, None),
        KeyBinding::new(sec!("5"), Connection5, None),
        KeyBinding::new(sec!("6"), Connection6, None),
        KeyBinding::new(sec!("7"), Connection7, None),
        KeyBinding::new(sec!("8"), Connection8, None),
        KeyBinding::new(sec!("9"), Connection9, None),
        KeyBinding::new(sec!("shift-l"), ToggleTheme, None),
        KeyBinding::new(sec!("q"), Quit, None),
        KeyBinding::new(sec!(","), OpenSettings, None),
        KeyBinding::new(sec!("="), ZoomIn, None),
        KeyBinding::new(sec!("+"), ZoomIn, None),
        KeyBinding::new(sec!("-"), ZoomOut, None),
        // Inline cell editing, TablePlus-style. Enter starts editing the
        // selected cell (the grid itself doesn't use Enter); inside the cell
        // editor these outrank the text input's own Esc / Tab bindings.
        KeyBinding::new("enter", EditCell, Some("EditableGrid > DataTable")),
        // In the grid, not inside a text field (where ⌥⌫ deletes a word).
        KeyBinding::new("alt-backspace", SetNull, Some("EditableGrid > DataTable")),
        // The bigger editor, from the grid or the inline editor.
        KeyBinding::new(
            "alt-enter",
            OpenCellEditor,
            Some("EditableGrid > DataTable"),
        ),
        KeyBinding::new("alt-enter", OpenCellEditor, Some("CellEditor > Input")),
        // In the cell editor: move through foreign-key matches, or step a
        // number (⇧: by 10). They outrank the text input's own ↑ / ↓.
        KeyBinding::new("up", CellUp, Some("CellEditor > Input")),
        KeyBinding::new("down", CellDown, Some("CellEditor > Input")),
        KeyBinding::new("shift-up", CellUpMore, Some("CellEditor > Input")),
        KeyBinding::new("shift-down", CellDownMore, Some("CellEditor > Input")),
        KeyBinding::new("escape", CancelCellEdit, Some("CellEditor > Input")),
        KeyBinding::new("tab", NextCell, Some("CellEditor > Input")),
        KeyBinding::new("shift-tab", PrevCell, Some("CellEditor > Input")),
        // The enum dropdown that replaces the text input.
        KeyBinding::new("up", ChoiceUp, Some("CellChoices")),
        KeyBinding::new("down", ChoiceDown, Some("CellChoices")),
        KeyBinding::new("enter", ChoicePick, Some("CellChoices")),
        KeyBinding::new("escape", CancelCellEdit, Some("CellEditor > CellChoices")),
        KeyBinding::new("tab", NextCell, Some("CellEditor > CellChoices")),
        KeyBinding::new("shift-tab", PrevCell, Some("CellEditor > CellChoices")),
        // Copy stays local to the grid so ⌘C in text fields copies text.
        KeyBinding::new(sec!("c"), CopyCell, Some("TableTab")),
        KeyBinding::new(
            "ctrl-space",
            gpui_component::input::ShowCompletions,
            Some("Input"),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use gpui::{Action, KeyBinding, KeyContext, Keymap, Keystroke};

    use super::*;

    /// The action a keystroke resolves to, given the focused element's
    /// context path (outermost first). `extra` bindings are registered first,
    /// standing in for the component library's own (e.g. the editor's).
    fn resolve(
        extra: Vec<KeyBinding>,
        key: &'static str,
        path: &[&str],
    ) -> Option<Box<dyn Action>> {
        let mut bindings = extra;
        bindings.extend(key_bindings());
        let keymap = Keymap::new(bindings);
        let stack: Vec<KeyContext> = path.iter().map(|c| KeyContext::parse(c).unwrap()).collect();
        let ks = Keystroke::parse(key).unwrap();
        keymap
            .bindings_for_input(&[ks], &stack)
            .0
            .first()
            .map(|b| b.action().boxed_clone())
    }

    fn is<A: Action>(a: Option<Box<dyn Action>>) -> bool {
        a.is_some_and(|a| a.as_any().is::<A>())
    }

    #[test]
    fn shortcuts_work_with_nothing_focused() {
        // Nothing focused: gpui dispatches from the window root, whose
        // element (gpui-component's Root) has the "Root" context.
        for (key, ok) in [
            (sec!("k"), is::<SwitchDatabase> as fn(_) -> bool),
            (sec!("t"), is::<NewQuery>),
            (sec!("w"), is::<CloseTab>),
            (sec!("r"), is::<Refresh>),
            (sec!("s"), is::<Commit>),
            (sec!("enter"), is::<RunQuery>),
            (sec!("."), is::<CancelQuery>),
            (sec!("y"), is::<QueryHistory>),
            (sec!("shift-e"), is::<ExportData>),
            (sec!("shift-i"), is::<ToggleRowDetail>),
            (sec!("shift-f"), is::<FormatSql>),
            (sec!("shift-s"), is::<SaveQuery>),
            (sec!("shift-o"), is::<SavedQueries>),
            (sec!("shift-p"), is::<ShowProcesses>),
            (sec!("e"), is::<ExplainQuery>),
            (sec!("f"), is::<ToggleFilter>),
        ] {
            assert!(ok(resolve(vec![], key, &["Root"])), "{key} with no focus");
            assert!(
                ok(resolve(vec![], key, &["Root", "App", "Session"])),
                "{key} in sidebar"
            );
        }
        assert!(is::<DeleteRows>(resolve(
            vec![],
            sec!("backspace"),
            &["Root"]
        )));
        assert!(is::<DeleteRows>(resolve(
            vec![],
            sec!("backspace"),
            &["Root", "App", "Session", "TableTab"]
        )));
        // ⌘C only copies a cell when the grid has focus.
        assert!(resolve(vec![], sec!("c"), &["Root"]).is_none());
        assert!(is::<CopyCell>(resolve(
            vec![],
            sec!("c"),
            &["Root", "App", "Session", "TableTab"]
        )));
    }

    #[test]
    fn editor_keeps_its_own_keys_except_run() {
        // Stand-ins for the editor's built-in bindings in the "Input" context.
        let editor = || {
            vec![
                KeyBinding::new(sec!("enter"), Quit, Some("Input")),
                KeyBinding::new(sec!("backspace"), Quit, Some("Input")),
                KeyBinding::new(sec!("f"), Quit, Some("Input")),
            ]
        };
        let in_editor = ["Root", "App", "Session", "QueryTab", "Input"];
        assert!(
            is::<RunQuery>(resolve(editor(), sec!("enter"), &in_editor)),
            "⌘↵ runs, no newline"
        );
        assert!(
            is::<Quit>(resolve(editor(), sec!("backspace"), &in_editor)),
            "⌘⌫ edits text"
        );
        assert!(
            is::<Quit>(resolve(editor(), sec!("f"), &in_editor)),
            "⌘F searches the editor"
        );
        // Global shortcuts the editor doesn't use still work from inside it.
        assert!(is::<SwitchDatabase>(resolve(
            editor(),
            sec!("k"),
            &in_editor
        )));
        assert!(is::<NewQuery>(resolve(editor(), sec!("t"), &in_editor)));
        // ⌘⇧K opens the connection picker; ⌘K stays the database picker.
        assert!(is::<OpenConnection>(resolve(
            editor(),
            sec!("shift-k"),
            &in_editor
        )));
        assert!(is::<OpenConnection>(resolve(
            vec![],
            sec!("shift-k"),
            &["Root"]
        )));
    }

    /// GitHub #12: dropdown search must actually filter.
    #[gpui::test]
    fn dropdown_search_filters(cx: &mut gpui::TestAppContext) {
        use gpui_component::searchable_list::SearchableListDelegate as _;
        let cx = cx.add_empty_window();
        cx.update(|window, cx| {
            let mut list = search_list(vec![
                "postgres".into(),
                "app_dev".into(),
                "analytics".into(),
            ]);
            let _ = list.perform_search("app", window, cx);
            assert_eq!(list.items_count(0), 1);
            let _ = list.perform_search("", window, cx);
            assert_eq!(list.items_count(0), 3);
        });
    }
}
