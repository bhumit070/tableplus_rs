//! TablePlus-style inline cell editing, shared by table tabs and query
//! results: Enter or typing starts editing the selected cell, Enter saves,
//! Tab / Shift+Tab save and move, Esc cancels, multi-line values open a
//! dialog. Enum and boolean columns get a dropdown of their values instead of
//! a text box. The text is checked against the column type as you type
//! (`cell_value`); ↵ / Tab won't save an invalid value. ⌥⌫ sets NULL, and
//! Space toggles a boolean. Edits are staged in the `Grid` until the host
//! commits them.

use gpui::{
    App, AppContext as _, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Render,
    ScrollHandle, StatefulInteractiveElement as _, Styled as _, Subscription, Window, anchored,
    deferred, div, prelude::FluentBuilder as _, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Editor, EditorState, InputEvent, InputState, Position};
use gpui_component::table::TableState;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, ThemeStyled as _, WindowExt as _, h_flex,
    v_flex,
};

use super::DisabledWhen as _;
use super::cell_value::{check, normalize_uuid};
use super::grid::{Grid, Kind, enum_pill};
use super::theme::{dp, tokens};
use super::{
    CancelCellEdit, ChoiceDown, ChoicePick, ChoiceUp, EditCell, NextCell, OpenCellEditor, PrevCell,
    SetNull,
};
use gpui_component::notification::Notification;
use gpui_kit_assets::IconName as GpuiKitAssetsIconName;
use serde_json::{Value, from_str, to_string_pretty};

/// A view that hosts an editable grid.
pub trait CellEditHost: Sized + 'static {
    fn grid(&self) -> &Entity<TableState<Grid>>;
    /// Holds the open editor's subscription (dropped when editing ends).
    fn edit_sub(&mut self) -> &mut Option<Subscription>;
}

/// How a cell edit starts.
pub enum EditStart {
    /// Enter / double-click: edit the current value, all selected.
    Keep,
    /// A typed character replaces the value.
    Typed(String),
}

/// The editor open on a cell.
pub enum CellEditor {
    Text(Entity<InputState>),
    Choice(Entity<Choices>),
}

/// A dropdown of a column's allowed values (`None` = NULL), opened in place
/// of the text box. Arrows move, Enter or a click picks, typing a letter
/// jumps to the next value starting with it.
pub struct Choices {
    options: Vec<Option<String>>,
    /// The value when editing started, marked with ✓.
    original: usize,
    ix: usize,
    /// Booleans show as true / false; enum values as pills.
    boolean: bool,
    focus: FocusHandle,
    scroll: ScrollHandle,
}

pub enum ChoiceEvent {
    /// Picked with Enter or a click.
    Picked,
}

impl EventEmitter<ChoiceEvent> for Choices {}

impl Focusable for Choices {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Choices {
    fn new(
        options: Vec<Option<String>>,
        current: &Option<String>,
        boolean: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let ix = match current {
            // "t" vs "true": match booleans by meaning.
            Some(v) if boolean => options
                .iter()
                .position(|o| o.as_deref().and_then(truthy) == truthy(v))
                .unwrap_or(0),
            _ => options.iter().position(|o| o == current).unwrap_or(0),
        };
        let scroll = ScrollHandle::new();
        scroll.scroll_to_item(ix);
        Self {
            options,
            original: ix,
            ix,
            boolean,
            focus: cx.focus_handle(),
            scroll,
        }
    }

    /// What an option is called: NULL, true / false, or the value.
    fn label(&self, o: &Option<String>) -> String {
        match o {
            None => "NULL".into(),
            Some(v) if self.boolean => match truthy(v) {
                Some(true) => "true".into(),
                Some(false) => "false".into(),
                None => v.clone(),
            },
            Some(v) => v.clone(),
        }
    }

    pub fn value(&self) -> Option<String> {
        self.options.get(self.ix).cloned().flatten()
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.ix = ix.min(self.options.len().saturating_sub(1));
        self.scroll.scroll_to_item(self.ix);
        cx.notify();
    }

    /// The next value (after the highlighted one, wrapping) that starts
    /// with `ch`, ignoring case.
    fn jump(&mut self, ch: &str, cx: &mut Context<Self>) {
        let ch = ch.to_lowercase();
        let n = self.options.len();
        let hit = (1..=n)
            .map(|d| (self.ix + d) % n)
            .find(|&i| self.label(&self.options[i]).to_lowercase().starts_with(&ch));
        if let Some(i) = hit {
            self.select(i, cx);
        }
    }
}

impl Render for Choices {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let items: Vec<_> = self
            .options
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let label = self.label(o);
                let content = match o {
                    None => div()
                        .text_size(dp(11.))
                        .text_color(t.text_muted)
                        .child("NULL")
                        .into_any_element(),
                    Some(v) if !self.boolean => enum_pill(v, cx).into_any_element(),
                    Some(_) => div().child(label).into_any_element(),
                };
                h_flex()
                    .id(i)
                    .h(dp(28.))
                    .px(dp(8.))
                    .gap(dp(8.))
                    .rounded(dp(6.))
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .when(i == self.ix, |d| d.bg(t.accent.opacity(0.16)))
                    .hover(|d| d.bg(t.hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.select(i, cx);
                            cx.emit(ChoiceEvent::Picked);
                        }),
                    )
                    .child(div().flex_1().child(content))
                    .child(
                        div()
                            .w(dp(12.))
                            .text_color(t.accent)
                            .when(i == self.original, |d| {
                                d.child(Icon::new(IconName::Check).xsmall())
                            }),
                    )
                    .when(i < 9, |d| {
                        d.child(
                            div()
                                .w(dp(10.))
                                .text_size(dp(11.))
                                .text_color(t.text_faint)
                                .child((i + 1).to_string()),
                        )
                    })
            })
            .collect();
        let menu = v_flex()
            .id("choices")
            .occlude()
            .mt(dp(8.))
            .p(dp(5.))
            .min_w(dp(180.))
            .max_h(dp(280.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .popover_style(cx)
            .font_family(cx.theme().font_family.clone())
            .text_size(dp(12.))
            .children(items)
            .child(
                div()
                    .mt(dp(4.))
                    .pt(dp(6.))
                    .px(dp(8.))
                    .border_t_1()
                    .border_color(t.border)
                    .text_size(dp(11.))
                    .text_color(t.text_faint)
                    .child("Type a letter to jump · 1–9 picks"),
            );
        let current = self.options.get(self.ix).cloned().flatten();
        h_flex()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .key_context("CellChoices")
            .on_action(
                cx.listener(|this, _: &ChoiceUp, _, cx| this.select(this.ix.saturating_sub(1), cx)),
            )
            .on_action(cx.listener(|this, _: &ChoiceDown, _, cx| this.select(this.ix + 1, cx)))
            .on_action(cx.listener(|_, _: &ChoicePick, _, cx| cx.emit(ChoiceEvent::Picked)))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                let m = &ev.keystroke.modifiers;
                let Some(ch) = ev.keystroke.key_char.as_deref() else {
                    return;
                };
                if m.platform
                    || m.control
                    || m.alt
                    || ch.chars().count() != 1
                    || ch.trim().is_empty()
                {
                    return;
                }
                cx.stop_propagation();
                match ch.parse::<usize>() {
                    // 1–9 pick by position.
                    Ok(d @ 1..=9) if d <= this.options.len() => {
                        this.select(d - 1, cx);
                        cx.emit(ChoiceEvent::Picked);
                    }
                    _ => this.jump(ch, cx),
                }
            }))
            .justify_between()
            .px(dp(8.))
            .child(match &current {
                Some(v) if !self.boolean => enum_pill(v, cx).into_any_element(),
                o => div().child(self.label(o)).into_any_element(),
            })
            .child(
                Icon::new(IconName::ChevronDown)
                    .xsmall()
                    .text_color(t.text_muted),
            )
            .child(
                div().absolute().left_0().top_full().child(
                    deferred(anchored().snap_to_window_with_margin(px(8.)).child(menu))
                        .with_priority(1),
                ),
            )
    }
}

/// "t", "true", "1", "yes" → true; "f", "false", "0", "no" → false.
pub fn truthy(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "t" | "true" | "1" | "yes" | "y" | "on" => Some(true),
        "f" | "false" | "0" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

pub fn is_editing<H: CellEditHost>(host: &H, cx: &App) -> bool {
    host.grid().read(cx).delegate().editing.is_some()
}

/// Opens the in-place editor on cell (r, c) if that column is editable.
pub fn start_edit<H: CellEditHost>(
    host: &mut H,
    r: usize,
    c: usize,
    start: EditStart,
    window: &mut Window,
    cx: &mut Context<H>,
) {
    finish_edit(host, window, cx);
    let g = host.grid().read(cx).delegate();
    if !g.can_edit(r, c) {
        return;
    }
    let Some(current) = g.value(r, c).cloned() else {
        return;
    };
    let column = g.columns.get(c).cloned().unwrap_or_default();
    let boolean = g.kind(c) == Kind::Bool;
    // JSON edits in the bigger dialog (⌥↵ opens it for any text).
    let json = g.kind(c) == Kind::Json;
    if let Some(options) = g.choices.get(c).filter(|o| !o.is_empty()).cloned() {
        let choices = cx.new(|cx| Choices::new(options, &current, boolean, cx));
        if let EditStart::Typed(ch) = &start {
            choices.update(cx, |ch_, cx| ch_.jump(ch, cx));
        }
        let fh = choices.focus_handle(cx);
        window.focus(&fh, cx);
        // Picking, or focus leaving the dropdown, saves the value.
        let picked = cx.subscribe_in(
            &choices,
            window,
            |host: &mut H, _, _: &ChoiceEvent, w, cx| finish_edit(host, w, cx),
        );
        let blurred = cx.on_blur(&fh, window, |host: &mut H, w, cx| finish_edit(host, w, cx));
        *host.edit_sub() = Some(Subscription::join(picked, blurred));
        host.grid().update(cx, |t, cx| {
            t.delegate_mut().editing = Some((r, c, CellEditor::Choice(choices)));
            cx.notify();
        });
        return;
    }
    let text = match &start {
        EditStart::Keep => current.unwrap_or_default(),
        EditStart::Typed(s) => s.clone(),
    };
    // A single-line editor would flatten newlines; edit those in a dialog.
    if matches!(start, EditStart::Keep) && (text.contains('\n') || json) {
        edit_multiline(host, r, c, column, text, window, cx);
        return;
    }
    let input = cx.new(|cx| InputState::new(window, cx).default_value(text.clone()));
    *host.edit_sub() = Some(cx.subscribe_in(
        &input,
        window,
        |host: &mut H, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => {
                pick_fk_match(host, window, cx);
                try_finish_edit(host, window, cx);
            }
            InputEvent::Blur => leave_edit(host, window, cx),
            // The check under the cell (and foreign-key matches) follow the
            // text.
            InputEvent::Change => {
                fk_search(host, window, cx);
                host.grid().update(cx, |_, cx| cx.notify())
            }
            _ => {}
        },
    ));
    input.update(cx, |i, cx| {
        i.focus(window, cx);
        match &start {
            // Like a spreadsheet: the whole value is selected, so typing
            // replaces it and the arrow keys move within it.
            EditStart::Keep => i.select_all(window, cx),
            EditStart::Typed(s) => {
                let end = Position::new(0, s.encode_utf16().count() as u32);
                i.set_cursor_position(end, window, cx);
            }
        }
    });
    host.grid().update(cx, |t, cx| {
        let g = t.delegate_mut();
        g.cal_month = None;
        g.fk_matches = None;
        g.fk_highlight = None;
        g.editing = Some((r, c, CellEditor::Text(input)));
        cx.notify();
    });
    fk_search(host, window, cx);
}

/// A foreign-key cell is being edited: ask the host for rows matching its
/// text.
fn fk_search<H: CellEditHost>(host: &mut H, window: &mut Window, cx: &mut Context<H>) {
    let g = host.grid().read(cx).delegate();
    let Some((_, c, CellEditor::Text(input))) = &g.editing else {
        return;
    };
    if !g.links.get(*c).copied().unwrap_or(false) {
        return;
    }
    let Some(search) = g.on_fk_search.clone() else {
        return;
    };
    let (c, text) = (*c, input.read(cx).value().trim().to_string());
    host.grid().update(cx, |t, _| {
        let g = t.delegate_mut();
        g.fk_matches = None;
        g.fk_highlight = None;
    });
    // Later: the host is in the middle of an update right now.
    window.defer(cx, move |window, cx| search(c, text, window, cx));
}

/// ↵ with a foreign-key match highlighted: that key becomes the value.
fn pick_fk_match<H: CellEditHost>(host: &mut H, window: &mut Window, cx: &mut Context<H>) {
    let g = host.grid().read(cx).delegate();
    let Some((_, _, CellEditor::Text(input))) = &g.editing else {
        return;
    };
    let Some(key) = g
        .fk_highlight
        .and_then(|i| g.fk_matches.as_ref()?.get(i))
        .map(|(k, _)| k.clone())
    else {
        return;
    };
    input
        .clone()
        .update(cx, |i, cx| i.set_value(key, window, cx));
}

/// Why the open text editor can't be saved, if it can't.
fn edit_error<H: CellEditHost>(host: &H, cx: &App) -> Option<String> {
    let g = host.grid().read(cx).delegate();
    let (_, _, CellEditor::Text(input)) = g.editing.as_ref()? else {
        return None;
    };
    let (r, c) = g.editing.as_ref().map(|(r, c, _)| (*r, *c))?;
    let text = input.read(cx).value();
    // Left empty on a NULL / DEFAULT cell: nothing changes (see finish_edit).
    if text.is_empty() && matches!(g.value(r, c), Some(None)) {
        return None;
    }
    let ty = g.types.get(c).cloned().unwrap_or_default();
    check(&ty, &text).error
}

/// ↵ / Tab: save, unless the value is invalid (the editor stays open and
/// says why). Returns whether it saved.
pub fn try_finish_edit<H: CellEditHost>(
    host: &mut H,
    window: &mut Window,
    cx: &mut Context<H>,
) -> bool {
    if edit_error(host, cx).is_some() {
        return false;
    }
    finish_edit(host, window, cx);
    true
}

/// Focus left the editor (a click elsewhere): save a valid value; drop an
/// invalid one rather than staging it, and say so.
fn leave_edit<H: CellEditHost>(host: &mut H, window: &mut Window, cx: &mut Context<H>) {
    match edit_error(host, cx) {
        None => finish_edit(host, window, cx),
        Some(e) => {
            cancel_edit(host, window, cx);
            window.push_notification(Notification::warning(format!("Not saved: {e}")), cx);
        }
    }
}

/// Saves the open editor's text into the grid (staged) and refocuses it.
pub fn finish_edit<H: CellEditHost>(host: &mut H, window: &mut Window, cx: &mut Context<H>) {
    let Some((r, c, editor)) = host
        .grid()
        .update(cx, |t, _| t.delegate_mut().editing.take())
    else {
        return;
    };
    *host.edit_sub() = None;
    host.grid().update(cx, |t, cx| {
        let g = t.delegate_mut();
        match editor {
            CellEditor::Text(input) => {
                let mut text = input.read(cx).value().to_string();
                // Typing into a NULL cell and leaving it empty keeps it NULL.
                let was_null = matches!(g.value(r, c), Some(None));
                if g.types
                    .get(c)
                    .is_some_and(|t| t.eq_ignore_ascii_case("uuid"))
                    && let Some(u) = normalize_uuid(&text)
                {
                    text = u;
                }
                if !(was_null && text.is_empty()) {
                    g.set_cell(r, c, Some(text));
                }
            }
            CellEditor::Choice(choices) => g.set_cell(r, c, choices.read(cx).value()),
        }
        cx.notify();
    });
    let fh = host.grid().focus_handle(cx);
    window.focus(&fh, cx);
    cx.notify();
}

/// Esc: drop the edit and keep the original value.
pub fn cancel_edit<H: CellEditHost>(host: &mut H, window: &mut Window, cx: &mut Context<H>) {
    let had = host.grid().update(cx, |t, cx| {
        cx.notify();
        t.delegate_mut().editing.take().is_some()
    });
    if had {
        *host.edit_sub() = None;
        let fh = host.grid().focus_handle(cx);
        window.focus(&fh, cx);
    }
}

/// Tab / Shift+Tab: save, then edit the next / previous editable cell,
/// wrapping across rows.
pub fn move_edit<H: CellEditHost>(
    host: &mut H,
    step: isize,
    window: &mut Window,
    cx: &mut Context<H>,
) {
    let Some((r, c)) = host
        .grid()
        .read(cx)
        .delegate()
        .editing
        .as_ref()
        .map(|(r, c, _)| (*r, *c))
    else {
        return;
    };
    if !try_finish_edit(host, window, cx) {
        return;
    }
    let g = host.grid().read(cx).delegate();
    let (rows, cols) = (g.rows.len() as isize, g.columns.len() as isize);
    let mut ix = r as isize * cols + c as isize;
    // Skip read-only columns (e.g. computed ones in query results).
    let next = loop {
        ix += step;
        if ix < 0 || ix >= rows * cols {
            break None;
        }
        let (nr, nc) = ((ix / cols) as usize, (ix % cols) as usize);
        if g.can_edit(nr, nc) {
            break Some((nr, nc));
        }
    };
    let Some((nr, nc)) = next else { return };
    host.grid().update(cx, |t, cx| {
        t.set_selected_cell(nr, nc, cx);
        t.scroll_to_row(nr, cx);
    });
    start_edit(host, nr, nc, EditStart::Keep, window, cx);
}

fn edit_multiline<H: CellEditHost>(
    host: &mut H,
    r: usize,
    c: usize,
    column: String,
    text: String,
    window: &mut Window,
    cx: &mut Context<H>,
) {
    let g = host.grid().read(cx).delegate();
    let ty = g.types.get(c).cloned().unwrap_or_default();
    let json = g.kind(c) == Kind::Json;
    let grid = host.grid().clone();
    let view = cx.new(|cx| CellDialog::new(grid, r, c, ty.clone(), json, text, window, cx));
    let title = if ty.is_empty() {
        format!("Edit {column}")
    } else {
        format!("Edit {column} · {ty}")
    };
    let first = std::cell::Cell::new(true);
    let width = px(760. * crate::settings::get(cx).ui_font_size / 13.);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            let editor = view.read(cx).editor.clone();
            editor.update(cx, |e, cx| e.focus(window, cx));
        }
        dialog.title(title.clone()).w(width).child(view.clone())
    });
}

/// The bigger editor for JSON and long text (design.md, Cell editors #9):
/// JSON is highlighted, can be formatted, and is checked as you type; Stage
/// is off while it's invalid. Set NULL sits apart on the left.
struct CellDialog {
    editor: Entity<EditorState>,
    grid: Entity<TableState<Grid>>,
    r: usize,
    c: usize,
    ty: String,
    json: bool,
    _sub: Subscription,
}

impl CellDialog {
    #[allow(clippy::too_many_arguments)]
    fn new(
        grid: Entity<TableState<Grid>>,
        r: usize,
        c: usize,
        ty: String,
        json: bool,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let e = EditorState::new(window, cx)
                .line_number(json)
                .soft_wrap(!json)
                .default_value(text);
            if json { e.language("json") } else { e }
        });
        let _sub = cx.observe(&editor, |_, _, cx| cx.notify());
        Self {
            editor,
            grid,
            r,
            c,
            ty,
            json,
            _sub,
        }
    }

    fn error(&self, cx: &App) -> Option<String> {
        check(&self.ty, &self.editor.read(cx).value()).error
    }

    fn stage(&mut self, value: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let (r, c) = (self.r, self.c);
        self.grid.update(cx, |t, cx| {
            t.delegate_mut().set_cell(r, c, value);
            cx.notify();
        });
        window.close_dialog(cx);
    }

    fn format(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        if let Ok(v) = from_str::<Value>(&text)
            && let Ok(pretty) = to_string_pretty(&v)
        {
            self.editor
                .update(cx, |e, cx| e.replace_all(pretty, window, cx));
        }
    }
}

impl Render for CellDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let text = self.editor.read(cx).value().to_string();
        let error = self.error(cx);
        let lines = text.lines().count().max(1);
        let status = format!(
            "{} bytes · {lines} line{}",
            text.len(),
            if lines == 1 { "" } else { "s" }
        );
        let editable = self.grid.read(cx).delegate().can_edit(self.r, self.c);
        v_flex()
            .gap(dp(10.))
            // ⌘↵ stages here, instead of running the query tab behind.
            .on_action(cx.listener(|this, _: &super::RunQuery, window, cx| {
                if this.error(cx).is_none() {
                    let v = this.editor.read(cx).value().to_string();
                    this.stage(Some(v), window, cx);
                }
            }))
            .child(
                h_flex()
                    .gap(dp(8.))
                    .when(self.json, |d| {
                        d.child(
                            Button::new("format-json")
                                .cursor_pointer()
                                .icon(GpuiKitAssetsIconName::Braces)
                                .label("Format")
                                .ghost()
                                .small()
                                .disabled_when(error.is_some())
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.format(window, cx)),
                                ),
                        )
                    })
                    .child(div().flex_1())
                    .when(self.json, |d| {
                        d.child(match &error {
                            None => h_flex()
                                .gap(dp(6.))
                                .text_size(dp(12.))
                                .text_color(t.success)
                                .child(div().size(dp(7.)).rounded_full().bg(t.success))
                                .child("Valid JSON"),
                            Some(_) => h_flex()
                                .gap(dp(6.))
                                .text_size(dp(12.))
                                .text_color(t.danger)
                                .child(div().size(dp(7.)).rounded_full().bg(t.danger))
                                .child("Invalid JSON"),
                        })
                    }),
            )
            .child(
                div()
                    .h(dp(360.))
                    .rounded(dp(8.))
                    .border_1()
                    .border_color(if error.is_some() { t.danger } else { t.border })
                    .bg(t.canvas)
                    .overflow_hidden()
                    .child(Editor::new(&self.editor).bordered(false).h_full()),
            )
            .child(
                h_flex()
                    .text_size(dp(11.5))
                    .text_color(if error.is_some() {
                        t.danger
                    } else {
                        t.text_muted
                    })
                    .child(error.unwrap_or(status)),
            )
            .child(
                h_flex()
                    .pt(dp(12.))
                    .gap(dp(8.))
                    .border_t_1()
                    .border_color(t.border)
                    .child(
                        Button::new("cell-null")
                            .cursor_pointer()
                            .label("Set NULL")
                            .ghost()
                            .small()
                            .disabled_when(!editable)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.stage(None, window, cx)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("cancel-cell")
                            .cursor_pointer()
                            .label("Cancel")
                            .outline()
                            .small()
                            .on_click(|_, w, cx| w.close_dialog(cx)),
                    )
                    .child(
                        Button::new("save-cell")
                            .cursor_pointer()
                            .label("Stage change  ⌘↵")
                            .primary()
                            .small()
                            .disabled_when(self.error(cx).is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                let v = this.editor.read(cx).value().to_string();
                                this.stage(Some(v), window, cx);
                            })),
                    ),
            )
    }
}

/// ⌥↵: edit the selected cell (or the one being edited) in the bigger
/// editor.
fn open_dialog<H: CellEditHost>(host: &mut H, window: &mut Window, cx: &mut Context<H>) {
    let g = host.grid().read(cx).delegate();
    let (r, c, text) = match &g.editing {
        Some((r, c, CellEditor::Text(input))) => (*r, *c, input.read(cx).value().to_string()),
        Some(_) => return,
        None => {
            let Some((r, c)) = host.grid().read(cx).selected_cell() else {
                return;
            };
            if !g.can_edit(r, c) {
                return;
            }
            (r, c, g.cell_string(r, c).unwrap_or_default())
        }
    };
    let column = g.columns.get(c).cloned().unwrap_or_default();
    cancel_edit(host, window, cx);
    edit_multiline(host, r, c, column, text, window, cx);
}

/// Typing a character while a cell is selected starts editing it with that
/// character, replacing the value (like a spreadsheet).
fn on_key_down<H: CellEditHost>(
    host: &mut H,
    ev: &KeyDownEvent,
    window: &mut Window,
    cx: &mut Context<H>,
) {
    let m = &ev.keystroke.modifiers;
    if m.platform || m.control || m.alt || m.function || is_editing(host, cx) {
        return;
    }
    // Focus within the grid (it or a cell), not in a filter box or editor.
    if !host.grid().focus_handle(cx).contains_focused(window, cx) {
        return;
    }
    let Some(ch) = ev.keystroke.key_char.clone().filter(|k| {
        let mut it = k.chars();
        matches!((it.next(), it.next()), (Some(c), None) if !c.is_control())
    }) else {
        return;
    };
    let Some((r, c)) = host.grid().read(cx).selected_cell() else {
        return;
    };
    if !host.grid().read(cx).delegate().can_edit(r, c) {
        return;
    }
    if ch == " " && host.grid().read(cx).delegate().kind(c) == Kind::Bool {
        window.prevent_default();
        cx.stop_propagation();
        toggle_bool(host, r, c, cx);
        return;
    }
    // The key becomes the editor's first character, not a second one.
    window.prevent_default();
    cx.stop_propagation();
    start_edit(host, r, c, EditStart::Typed(ch), window, cx);
}

/// Space on a boolean cell: flip it (NULL becomes true), in the column's
/// own spelling ("true" / "1"…).
fn toggle_bool<H: CellEditHost>(host: &mut H, r: usize, c: usize, cx: &mut Context<H>) {
    host.grid().update(cx, |t, cx| {
        let g = t.delegate_mut();
        let now = g.cell_string(r, c).as_deref().and_then(truthy);
        let want = !now.unwrap_or(false);
        let pick = g
            .choices
            .get(c)
            .and_then(|o| {
                o.iter()
                    .flatten()
                    .find(|v| truthy(v) == Some(want))
                    .cloned()
            })
            .unwrap_or_else(|| want.to_string());
        g.set_cell(r, c, Some(pick));
        cx.notify();
    });
}

/// ⌥⌫: set the selected cell to NULL (staged).
pub fn set_null<H: CellEditHost>(host: &mut H, cx: &mut Context<H>) {
    let Some((r, c)) = host.grid().read(cx).selected_cell() else {
        return;
    };
    host.grid().update(cx, |t, cx| {
        let g = t.delegate_mut();
        if g.can_edit(r, c) {
            g.set_cell(r, c, None);
            cx.notify();
        }
    });
}

/// Wraps a grid element with the editing keys. The "EditableGrid" context
/// scopes Enter-to-edit to grids that opt in.
pub fn editable<H: CellEditHost>(grid: impl IntoElement, cx: &mut Context<H>) -> Div {
    div()
        .size_full()
        .key_context("EditableGrid")
        .capture_key_down(cx.listener(|h: &mut H, ev, w, cx| on_key_down(h, ev, w, cx)))
        .on_action(cx.listener(|h: &mut H, _: &EditCell, w, cx| {
            // The cell editor passes Enter on after saving; don't reopen it.
            if is_editing(h, cx) {
                return;
            }
            if let Some((r, c)) = h.grid().read(cx).selected_cell() {
                start_edit(h, r, c, EditStart::Keep, w, cx);
            }
        }))
        .on_action(cx.listener(|h: &mut H, _: &CancelCellEdit, w, cx| cancel_edit(h, w, cx)))
        .on_action(cx.listener(|h: &mut H, _: &SetNull, _, cx| set_null(h, cx)))
        .on_action(cx.listener(|h: &mut H, _: &OpenCellEditor, w, cx| open_dialog(h, w, cx)))
        .on_action(cx.listener(|h: &mut H, _: &NextCell, w, cx| move_edit(h, 1, w, cx)))
        .on_action(cx.listener(|h: &mut H, _: &PrevCell, w, cx| move_edit(h, -1, w, cx)))
        .child(grid)
}
