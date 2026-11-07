//! Tabula's look: the bundled Geist fonts and the Tabula Dark / Light themes.
//! The themes are built from the design tokens in design.md by
//! `assets/themes/tabula.py`, which writes `assets/themes/tabula.json`.

use std::borrow::Cow;

use gpui::App;
use gpui_component::{Theme, ThemeRegistry};

const THEMES: &str = include_str!("../../assets/themes/tabula.json");
const DARK: &str = "Tabula Dark";
const LIGHT: &str = "Tabula Light";

/// Geist and Geist Mono (SIL OFL 1.1, `assets/fonts/OFL.txt`).
const FONTS: [&[u8]; 8] = [
    include_bytes!("../../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/Geist-Bold.ttf"),
    include_bytes!("../../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../../assets/fonts/GeistMono-Medium.ttf"),
    include_bytes!("../../assets/fonts/GeistMono-SemiBold.ttf"),
    include_bytes!("../../assets/fonts/GeistMono-Bold.ttf"),
];

/// Registers the fonts and makes the Tabula themes the ones light and dark
/// mode use. Call after `gpui_component::init` and before choosing a mode
/// (`Theme::sync_system_appearance`), which applies them.
pub fn init(cx: &mut App) {
    cx.text_system()
        .add_fonts(FONTS.iter().map(|f| Cow::Borrowed(*f)).collect())
        .expect("load the bundled Geist fonts");
    let registry = ThemeRegistry::global_mut(cx);
    registry
        .load_themes_from_str(THEMES)
        .expect("parse assets/themes/tabula.json");
    let dark = registry.themes()[DARK].clone();
    let light = registry.themes()[LIGHT].clone();
    Theme::update(cx, |theme| {
        theme.dark_theme = dark;
        theme.light_theme = light;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themes_parse_and_every_colour_is_valid() {
        let set: gpui_component::ThemeSet = serde_json::from_str(THEMES).unwrap();
        let names: Vec<_> = set.themes.iter().map(|t| t.name.to_string()).collect();
        assert_eq!(names, [DARK, LIGHT]);
        let json: serde_json::Value = serde_json::from_str(THEMES).unwrap();
        for theme in json["themes"].as_array().unwrap() {
            for (key, value) in theme["colors"].as_object().unwrap() {
                let value = value.as_str().unwrap();
                assert!(
                    gpui::Rgba::try_from(value).is_ok(),
                    "{key} = {value} isn't a colour"
                );
            }
        }
    }
}
