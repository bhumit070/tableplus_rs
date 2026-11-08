//! The safe-mode confirmation before a write (design.md, screen 11): the
//! environment in its colour, the exact SQL, what it changes, and a button
//! that says what happens and where. On production ↵ cancels and only ⌘↵
//! (or a click) confirms. "Confirm writes" can be snoozed for ten minutes
//! per connection, in memory only.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, ClipboardItem, Context, FocusHandle, Focusable, FontWeight, Global,
    IntoElement, KeyDownEvent, ParentElement as _, Render, Styled as _, Window, div, prelude::*,
    px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit_assets::IconName;

use super::theme::{dp, env_color, env_short, tokens};
use crate::db::{ConnectionConfig, EnvTag, SafeMode};

const SNOOZE: Duration = Duration::from_secs(10 * 60);

/// Connections whose "Confirm writes" was snoozed, and when.
#[derive(Default)]
struct Snoozed(HashMap<String, Instant>);

impl Global for Snoozed {}

/// Whether writes on `cfg` skip the confirmation right now.
pub fn snoozed(cfg: &ConnectionConfig, cx: &App) -> bool {
    cfg.safety() == SafeMode::ConfirmWrites
        && cx
            .try_global::<Snoozed>()
            .and_then(|s| s.0.get(&cfg.id))
            .is_some_and(|at| at.elapsed() < SNOOZE)
}

/// "1 updated · 1 deleted · 1 inserted" from the statements about to run;
/// other statements are counted as such.
pub fn summary(statements: &[String]) -> String {
    let (mut upd, mut del, mut ins, mut other) = (0, 0, 0, 0);
    for s in statements {
        let first = s
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match first.as_str() {
            "update" => upd += 1,
            "delete" => del += 1,
            "insert" => ins += 1,
            "" => {}
            _ => other += 1,
        }
    }
    let plural = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    let mut parts = vec![];
    if upd > 0 {
        parts.push(format!("{upd} updated"));
    }
    if del > 0 {
        parts.push(format!("{del} deleted"));
    }
    if ins > 0 {
        parts.push(format!("{ins} inserted"));
    }
    if other > 0 {
        parts.push(plural(other, "statement", "statements"));
    }
    parts.join(" · ")
}

/// DROP / TRUNCATE (or ALTER … DROP): the button turns red.
pub fn destructive(statements: &[String]) -> bool {
    statements.iter().any(|s| {
        let l = s.trim_start().to_ascii_lowercase();
        l.starts_with("drop ")
            || l.starts_with("truncate ")
            || (l.starts_with("alter ") && l.contains(" drop "))
    })
}

pub struct ConfirmWrite {
    cfg: ConnectionConfig,
    what: String,
    sql: String,
    statements: Vec<String>,
    snooze: bool,
    go: Rc<dyn Fn(&mut Window, &mut App)>,
    focus: FocusHandle,
}

impl ConfirmWrite {
    /// Opens the confirmation; `go` runs if the user confirms.
    pub fn open(
        cfg: &ConnectionConfig,
        what: &str,
        sql: String,
        window: &mut Window,
        cx: &mut App,
        go: Rc<dyn Fn(&mut Window, &mut App)>,
    ) {
        let backslashes = cfg.driver == crate::db::Driver::MySql;
        let statements = crate::db::split::split_statements(&sql, backslashes);
        let view = cx.new(|cx| Self {
            cfg: cfg.clone(),
            what: what.to_string(),
            sql,
            statements,
            snooze: false,
            go,
            focus: cx.focus_handle(),
        });
        let width = px(640. * crate::settings::get(cx).ui_font_size / 13.);
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                view.read(cx).focus.clone().focus(window, cx);
            }
            // gpui-component binds ↵ to the dialog's OK, and bindings run
            // before our key handler: decide here. Production cancels;
            // elsewhere ↵ confirms (confirm closes the dialog itself).
            let on_enter = view.clone();
            dialog
                .w(width)
                .close_button(false)
                .on_ok(move |_, window, cx| {
                    on_enter.update(cx, |this, cx| {
                        if this.production() {
                            true
                        } else {
                            this.confirm(window, cx);
                            false
                        }
                    })
                })
                .child(view.clone())
        });
    }

    fn production(&self) -> bool {
        self.cfg.tag == EnvTag::Production
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.snooze {
            cx.default_global::<Snoozed>()
                .0
                .insert(self.cfg.id.clone(), Instant::now());
        }
        window.close_dialog(cx);
        (self.go)(window, cx);
    }
}

impl Focusable for ConfirmWrite {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ConfirmWrite {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = tokens(cx);
        let env = env_color(self.cfg.tag, cx);
        let prod = self.production();
        let red = prod || destructive(&self.statements);
        let target = match self.cfg.tag {
            EnvTag::Production => "prod".to_string(),
            _ => self.cfg.display_name(),
        };
        let where_ = if self.cfg.database.is_empty() {
            self.cfg.display_name()
        } else {
            format!("{} · {}", self.cfg.display_name(), self.cfg.database)
        };
        // "Commit to prod", "Run on prod".
        let prep = if self.what.starts_with("Commit") {
            "to"
        } else {
            "on"
        };
        let summary = summary(&self.statements);
        let can_snooze = self.cfg.safety() == SafeMode::ConfirmWrites;
        let sql = self.sql.clone();
        v_flex()
            .track_focus(&self.focus)
            .key_context("ConfirmWrite")
            .gap(dp(14.))
            // ⌘↵ confirms everywhere; plain ↵ only away from production.
            .on_action(
                cx.listener(|this, _: &super::RunQuery, window, cx| this.confirm(window, cx)),
            )
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                if k.key == "enter" && !k.modifiers.platform && !k.modifiers.control {
                    cx.stop_propagation();
                    if this.production() {
                        window.close_dialog(cx);
                    } else {
                        this.confirm(window, cx);
                    }
                }
            }))
            .child(
                h_flex()
                    .px(dp(14.))
                    .py(dp(10.))
                    .gap(dp(8.))
                    .rounded(dp(8.))
                    .bg(env.opacity(0.14))
                    .border_1()
                    .border_color(env.opacity(0.35))
                    .text_size(dp(13.))
                    .text_color(env)
                    .child(Icon::new(IconName::TriangleAlert).small())
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(match self.cfg.tag {
                                EnvTag::Production => "You are writing to PRODUCTION".to_string(),
                                tag => format!("You are writing to {}", env_short(tag)),
                            }),
                    )
                    .child(div().flex_1())
                    .child(div().text_size(dp(12.)).child(where_)),
            )
            .child(
                v_flex()
                    .gap(dp(4.))
                    .child(
                        div()
                            .text_size(dp(17.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{}?", self.what)),
                    )
                    .child(
                        div()
                            .text_size(dp(12.5))
                            .text_color(t.text_muted)
                            .child(format!(
                                "Safe mode: {}. Check what will run.",
                                match self.cfg.safety() {
                                    SafeMode::ConfirmAll => "confirm every query",
                                    _ => "confirm writes",
                                }
                            )),
                    ),
            )
            .child(
                v_flex()
                    .relative()
                    .rounded(dp(8.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.canvas)
                    .child(
                        div()
                            .id("confirm-sql")
                            .max_h(dp(260.))
                            .overflow_y_scroll()
                            .p(dp(12.))
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(dp(12.5))
                            .child(if self.sql.trim().is_empty() {
                                "(no statements)".to_string()
                            } else {
                                self.sql.clone()
                            }),
                    )
                    .child(
                        h_flex()
                            .px(dp(12.))
                            .py(dp(6.))
                            .border_t_1()
                            .border_color(t.border)
                            .text_size(dp(11.5))
                            .text_color(t.text_muted)
                            .child(summary)
                            .child(div().flex_1())
                            .child(
                                Button::new("copy-sql")
                                    .cursor_pointer()
                                    .icon(IconName::Copy)
                                    .label("Copy")
                                    .ghost()
                                    .xsmall()
                                    .on_click(move |_, window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            sql.clone(),
                                        ));
                                        window.push_notification("SQL copied", cx);
                                    }),
                            ),
                    ),
            )
            .when(can_snooze, |d| {
                d.child(
                    div().debug_selector(|| "snooze".into()).child(
                        Checkbox::new("snooze")
                            .cursor_pointer()
                            .checked(self.snooze)
                            .label("Don't ask again for this connection for 10 minutes")
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.snooze = *checked;
                                cx.notify();
                            })),
                    ),
                )
            })
            .child(
                h_flex()
                    .pt(dp(12.))
                    .gap(dp(8.))
                    .border_t_1()
                    .border_color(t.border)
                    .child(
                        div()
                            .text_size(dp(11.5))
                            .text_color(t.text_faint)
                            .child(if prod {
                                format!("↵ cancels · ⌘↵ {} {prep} prod", self.what.to_lowercase())
                            } else {
                                "↵ or ⌘↵ confirms · Esc cancels".to_string()
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("confirm-cancel")
                            .cursor_pointer()
                            .label("Cancel")
                            .outline()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("confirm-ok")
                            .cursor_pointer()
                            .label(format!("{} {prep} {target}", self.what))
                            .map(|b| if red { b.danger() } else { b.primary() })
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stmts(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn summary_counts_by_kind() {
        assert_eq!(
            summary(&stmts(&[
                "UPDATE orders SET status = 'paid' WHERE id = 1",
                "DELETE FROM orders WHERE id = 2",
                "INSERT INTO orders (id) VALUES (3)",
            ])),
            "1 updated · 1 deleted · 1 inserted"
        );
        assert_eq!(
            summary(&stmts(&["update a set x = 1", "update b set y = 2"])),
            "2 updated"
        );
        assert_eq!(summary(&stmts(&["ALTER TABLE t ADD c int"])), "1 statement");
        assert_eq!(summary(&[]), "");
    }

    #[test]
    fn destructive_statements() {
        assert!(destructive(&stmts(&["DROP TABLE t"])));
        assert!(destructive(&stmts(&["truncate t"])));
        assert!(destructive(&stmts(&["ALTER TABLE t DROP COLUMN c"])));
        assert!(!destructive(&stmts(&["ALTER TABLE t ADD c int"])));
        assert!(!destructive(&stmts(&["DELETE FROM t WHERE id = 1"])));
    }
}
