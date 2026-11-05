pub mod app;
pub mod completion;
pub mod conn_form;
pub mod db_picker;
pub mod grid;
pub mod home;
pub mod query_tab;
pub mod session;
pub mod settings_view;
pub mod table_tab;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Hsla, KeyBinding, rgb};
use tokio::sync::RwLock;

use crate::db::{Connection, ConnectionConfig};

gpui::actions!(
    tabula,
    [
        RunQuery,
        SwitchDatabase,
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
        GoHome,
        NewConnection,
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

pub fn tag_color(cfg: &ConnectionConfig) -> Hsla {
    rgb(cfg.tag.color()).into()
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
        KeyBinding::new(sec!("k"), SwitchDatabase, None),
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
        KeyBinding::new(sec!("n"), NewConnection, None),
        KeyBinding::new(sec!("shift-l"), ToggleTheme, None),
        KeyBinding::new(sec!("q"), Quit, None),
        KeyBinding::new(sec!(","), OpenSettings, None),
        KeyBinding::new(sec!("="), ZoomIn, None),
        KeyBinding::new(sec!("+"), ZoomIn, None),
        KeyBinding::new(sec!("-"), ZoomOut, None),
        // Copy stays local to the grid so ⌘C in text fields copies text.
        KeyBinding::new(sec!("c"), CopyCell, Some("TableTab")),
        KeyBinding::new(
            "ctrl-space",
            gpui_component::input::ShowCompletions,
            Some("Input"),
        ),
    ]
}

/// SELECT-like statements skip the production safe-mode prompt.
pub fn is_read_only(sql: &str) -> bool {
    let first = sql
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("--"))
        .unwrap_or("")
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        first.as_str(),
        "select"
            | "show"
            | "explain"
            | "describe"
            | "desc"
            | "pragma"
            | "with"
            | "values"
            | "table"
            | "db.find"
            | "get"
            | "keys"
            | "scan"
            | "hgetall"
            | "type"
            | "ttl"
            | "info"
    ) || first.starts_with("db.")
        && (sql.contains(".find(") || sql.contains(".aggregate(") || sql.contains(".count"))
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
    }
}
