//! User preferences, saved to `settings.json` next to `connections.json`.

use std::path::PathBuf;

use gpui::{App, Global, px};
use gpui_component::Theme;
use serde::{Deserialize, Serialize};

pub const UI_RANGE: (f32, f32) = (12., 24.);
pub const EDITOR_RANGE: (f32, f32) = (10., 28.);

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Base size the whole interface scales from (tables, sidebar, dialogs).
    pub ui_font_size: f32,
    /// SQL / query editor text.
    pub editor_font_size: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ui_font_size: 16.,
            editor_font_size: 13.,
        }
    }
}

impl Global for Settings {}

fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tabula")
        .join("settings.json")
}

impl Settings {
    fn load() -> Self {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
            .unwrap_or_default()
            .clamped()
    }

    fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default());
    }

    fn clamped(mut self) -> Self {
        self.ui_font_size = self.ui_font_size.round().clamp(UI_RANGE.0, UI_RANGE.1);
        self.editor_font_size = self
            .editor_font_size
            .round()
            .clamp(EDITOR_RANGE.0, EDITOR_RANGE.1);
        self
    }
}

pub fn get(cx: &App) -> Settings {
    cx.try_global::<Settings>().copied().unwrap_or_default()
}

/// Loads saved settings and applies them; call once after `gpui_component::init`.
pub fn init(cx: &mut App) {
    cx.set_global(Settings::load());
    apply_theme(cx);
}

/// Pushes the font sizes into the theme. Call again after a theme change,
/// which may reload sizes from the theme file.
pub fn apply_theme(cx: &mut App) {
    let s = get(cx);
    let theme = Theme::global_mut(cx);
    theme.font_size = px(s.ui_font_size);
    theme.mono_font_size = px(s.editor_font_size);
    cx.refresh_windows();
}

pub fn update(cx: &mut App, f: impl FnOnce(&mut Settings)) {
    let mut s = get(cx);
    f(&mut s);
    let s = s.clamped();
    s.save();
    cx.set_global(s);
    apply_theme(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_and_tolerates_partial_files() {
        let s: Settings = serde_json::from_str(r#"{"ui_font_size": 99}"#).unwrap();
        assert_eq!(
            s.clamped(),
            Settings {
                ui_font_size: 24.,
                editor_font_size: 13.
            }
        );
        let s = Settings {
            ui_font_size: 14.6,
            editor_font_size: 1.,
        }
        .clamped();
        assert_eq!((s.ui_font_size, s.editor_font_size), (15., 10.));
    }
}
