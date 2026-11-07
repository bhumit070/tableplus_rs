//! User preferences, saved to `settings.json` next to `connections.json`.

use std::path::PathBuf;

use gpui::{App, Global, px};
use gpui_component::{ActiveTheme as _, Theme, ThemeMode};
use serde::{Deserialize, Serialize};

use crate::db::SafeMode;
use crate::store::{config_dir, read_json, write_atomic};
use anyhow::Result;
use gpui_component::notification::Notification;
use serde_json::to_string_pretty;

pub const UI_RANGE: (f32, f32) = (11., 22.);
pub const EDITOR_RANGE: (f32, f32) = (10., 28.);
/// Rows per page a table tab can show.
pub const PAGE_SIZES: [usize; 4] = [100, 300, 1000, 5000];

/// Light, dark, or whatever the system uses at launch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Base size the whole interface scales from (tables, sidebar, dialogs).
    pub ui_font_size: f32,
    /// SQL / query editor text.
    pub editor_font_size: f32,
    /// Reopen last time's connections and tabs at launch.
    pub restore_sessions: bool,
    pub appearance: Appearance,
    /// Rows per page when a table opens (one of `PAGE_SIZES`).
    pub page_size: usize,
    /// Safe mode a new connection starts with.
    pub default_safe_mode: SafeMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // The design's body size (design.md, Typography).
            ui_font_size: 13.,
            editor_font_size: 13.,
            restore_sessions: true,
            appearance: Appearance::System,
            page_size: PAGE_SIZES[1],
            default_safe_mode: SafeMode::Auto,
        }
    }
}

impl Global for Settings {}

fn path() -> PathBuf {
    config_dir().join("settings.json")
}

impl Settings {
    /// A damaged file is moved aside and defaults are used.
    fn load() -> Self {
        let (s, warning): (Settings, _) = read_json(&path(), "settings");
        if let Some(w) = warning {
            eprintln!("{w}");
        }
        s.clamped()
    }

    fn save(&self) -> Result<()> {
        write_atomic(&path(), &to_string_pretty(self)?)
    }

    fn clamped(mut self) -> Self {
        self.ui_font_size = self.ui_font_size.round().clamp(UI_RANGE.0, UI_RANGE.1);
        self.editor_font_size = self
            .editor_font_size
            .round()
            .clamp(EDITOR_RANGE.0, EDITOR_RANGE.1);
        if !PAGE_SIZES.contains(&self.page_size) {
            self.page_size = PAGE_SIZES[1];
        }
        self
    }
}

pub fn get(cx: &App) -> Settings {
    cx.try_global::<Settings>().copied().unwrap_or_default()
}

/// Loads saved settings and applies them; call once after `gpui_component::init`.
pub fn init(cx: &mut App) {
    cx.set_global(Settings::load());
    apply_appearance(cx);
    apply_theme(cx);
}

/// Switches to the saved light / dark choice (System keeps what the system
/// uses, chosen at launch).
pub fn apply_appearance(cx: &mut App) {
    let mode = match get(cx).appearance {
        Appearance::System => return,
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
    };
    if cx.theme().mode != mode {
        Theme::change(mode, None, cx);
    }
}

/// Pushes the font sizes into the theme. Call again after a theme change,
/// which may reload sizes from the theme file.
pub fn apply_theme(cx: &mut App) {
    let s = get(cx);
    // `update`, not `global_mut`: it also pushes the change to the layer
    // that paints scrollbars, and refreshes the windows.
    Theme::update(cx, |theme| {
        theme.font_size = px(s.ui_font_size);
        theme.mono_font_size = px(s.editor_font_size);
    });
}

pub fn update(cx: &mut App, f: impl FnOnce(&mut Settings)) {
    let mut s = get(cx);
    f(&mut s);
    let s = s.clamped();
    if let Err(e) = s.save() {
        // Applied for now, but it won't survive a restart: say so.
        let msg = format!("Settings couldn't be saved: {e:#}");
        eprintln!("{msg}");
        // Deferred: this often runs inside a window's event handling, where
        // that window can't be updated again.
        cx.defer(move |cx| {
            if let Some(w) = cx.windows().first().copied() {
                let _ = w.update(cx, |_, window, cx| {
                    use gpui_component::WindowExt as _;
                    window.push_notification(Notification::warning(msg), cx);
                });
            }
        });
    }
    cx.set_global(s);
    apply_theme(cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::from_str;

    #[test]
    fn clamps_and_tolerates_partial_files() {
        let s: Settings = from_str(r#"{"ui_font_size": 99}"#).unwrap();
        assert_eq!(
            s.clamped(),
            Settings {
                ui_font_size: UI_RANGE.1,
                editor_font_size: 13.,
                ..Default::default()
            }
        );
        let s = Settings {
            ui_font_size: 14.6,
            editor_font_size: 1.,
            ..Default::default()
        }
        .clamped();
        assert_eq!((s.ui_font_size, s.editor_font_size), (15., 10.));
        let s: Settings = from_str(r#"{"page_size": 7}"#).unwrap();
        assert_eq!(s.clamped().page_size, 300);
    }
}
