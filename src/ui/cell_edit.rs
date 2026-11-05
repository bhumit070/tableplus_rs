//! TablePlus-style inline cell editing, shared by table tabs and query
//! results: Enter or typing starts editing the selected cell, Enter saves,
//! Tab / Shift+Tab save and move, Esc cancels, multi-line values open a
//! dialog. Edits are staged in the `Grid` until the host commits them.

use gpui::{
    App, AppContext as _, Context, Div, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::DialogFooter;
use gpui_component::input::{InputEvent, InputState, Position, Textarea, TextareaState};
use gpui_component::table::TableState;
use gpui_component::{Sizable as _, WindowExt as _};

use super::grid::Grid;
use super::{CancelCellEdit, EditCell, NextCell, PrevCell};

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
    let text = match &start {
        EditStart::Keep => current.unwrap_or_default(),
        EditStart::Typed(s) => s.clone(),
    };
    // A single-line editor would flatten newlines; edit those in a dialog.
    if matches!(start, EditStart::Keep) && text.contains('\n') {
        edit_multiline(host, r, c, column, text, window, cx);
        return;
    }
    let input = cx.new(|cx| InputState::new(window, cx).default_value(text.clone()));
    *host.edit_sub() = Some(cx.subscribe_in(
        &input,
        window,
        |host: &mut H, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } | InputEvent::Blur => finish_edit(host, window, cx),
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
        t.delegate_mut().editing = Some((r, c, input));
        cx.notify();
    });
}

/// Saves the open editor's text into the grid (staged) and refocuses it.
pub fn finish_edit<H: CellEditHost>(host: &mut H, window: &mut Window, cx: &mut Context<H>) {
    let Some((r, c, input)) = host
        .grid()
        .update(cx, |t, _| t.delegate_mut().editing.take())
    else {
        return;
    };
    *host.edit_sub() = None;
    let text = input.read(cx).value().to_string();
    host.grid().update(cx, |t, cx| {
        let g = t.delegate_mut();
        // Typing into a NULL cell and leaving it empty keeps it NULL.
        let was_null = matches!(g.value(r, c), Some(None));
        if !(was_null && text.is_empty()) {
            g.set_cell(r, c, Some(text));
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
    finish_edit(host, window, cx);
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
    let area = cx.new(|cx| TextareaState::new(window, cx).rows(10).default_value(text));
    let grid = host.grid().clone();
    let first = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            area.update(cx, |a, cx| a.focus(window, cx));
        }
        let (grid, area2) = (grid.clone(), area.clone());
        dialog
            .title(format!("Edit {column}"))
            .w(px(620.))
            .child(Textarea::new(&area))
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("cancel-cell")
                            .label("Cancel")
                            .small()
                            .on_click(|_, w, cx| w.close_dialog(cx)),
                    )
                    .child(
                        Button::new("save-cell")
                            .label("Save")
                            .primary()
                            .small()
                            .on_click(move |_, window, cx| {
                                let v = area2.read(cx).value().to_string();
                                grid.update(cx, |t, cx| {
                                    t.delegate_mut().set_cell(r, c, Some(v));
                                    cx.notify();
                                });
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
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
    // The key becomes the editor's first character, not a second one.
    window.prevent_default();
    cx.stop_propagation();
    start_edit(host, r, c, EditStart::Typed(ch), window, cx);
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
        .on_action(cx.listener(|h: &mut H, _: &NextCell, w, cx| move_edit(h, 1, w, cx)))
        .on_action(cx.listener(|h: &mut H, _: &PrevCell, w, cx| move_edit(h, -1, w, cx)))
        .child(grid)
}
