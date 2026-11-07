//! Tabula's look: the bundled Geist fonts, the Tabula Dark / Light themes,
//! the raw design tokens, and small building blocks from design.md
//! (Components). The themes and tokens are built from the design tokens by
//! `assets/themes/tabula.py`, which writes `assets/themes/tabula.json`.

use std::borrow::Cow;
use std::collections::HashMap;

use gpui::{
    App, Div, FontWeight, Hsla, ParentElement as _, Rems, Rgba, SharedString, Styled as _, div,
    rems,
};
use gpui_component::{ActiveTheme as _, Theme, ThemeRegistry};
use gpui_kit_assets::IconName;

use crate::db::{ConnectionConfig, Driver, EnvTag, SafeMode};

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

/// The design tokens for one mode (design.md, Colour tokens).
#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub canvas: Hsla,
    pub rail: Hsla,
    pub sidebar: Hsla,
    pub surface: Hsla,
    pub raised: Hsla,
    pub hover: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_muted: Hsla,
    pub text_faint: Hsla,
    pub accent: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
    pub info: Hsla,
    pub keyword: Hsla,
    pub number: Hsla,
}

impl Tokens {
    fn parse(map: &HashMap<String, String>) -> Self {
        let c = |key: &str| -> Hsla {
            let hex = map
                .get(key)
                .unwrap_or_else(|| panic!("token {key} missing from tabula.json"));
            Rgba::try_from(hex.as_str())
                .unwrap_or_else(|_| panic!("token {key} = {hex} isn't a colour"))
                .into()
        };
        Self {
            canvas: c("bg/canvas"),
            rail: c("bg/rail"),
            sidebar: c("bg/sidebar"),
            surface: c("bg/surface"),
            raised: c("bg/raised"),
            hover: c("bg/hover"),
            border: c("border/default"),
            text: c("text/primary"),
            text_secondary: c("text/secondary"),
            text_muted: c("text/muted"),
            text_faint: c("text/faint"),
            accent: c("accent"),
            success: c("success"),
            warning: c("warning"),
            danger: c("danger"),
            info: c("info"),
            keyword: c("keyword"),
            number: c("number"),
        }
    }
}

struct TokenSets {
    dark: Tokens,
    light: Tokens,
}

/// Parsed once, on first use.
static TOKEN_SETS: std::sync::LazyLock<TokenSets> = std::sync::LazyLock::new(|| {
    #[derive(serde::Deserialize)]
    struct File {
        tokens: HashMap<String, HashMap<String, String>>,
    }
    let file: File = serde_json::from_str(THEMES).expect("parse assets/themes/tabula.json");
    TokenSets {
        dark: Tokens::parse(&file.tokens["dark"]),
        light: Tokens::parse(&file.tokens["light"]),
    }
});

/// The tokens for the current light / dark mode.
pub fn tokens(cx: &App) -> Tokens {
    if cx.theme().mode.is_dark() {
        TOKEN_SETS.dark
    } else {
        TOKEN_SETS.light
    }
}

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

/// A size from the design, given in pixels at the default 13px interface
/// size. It's in rems, which follow the interface size setting (zoom), so
/// layouts keep their proportions.
pub fn dp(px: f32) -> Rems {
    rems(px / 13.)
}

/// The colour of an environment tag (design.md, Environment tags).
pub fn env_color(tag: EnvTag, cx: &App) -> Hsla {
    let t = tokens(cx);
    match tag {
        EnvTag::Local => t.success,
        EnvTag::Development => t.info,
        EnvTag::Staging => t.warning,
        EnvTag::Production => t.danger,
    }
}

/// The text on an environment pill.
pub fn env_short(tag: EnvTag) -> &'static str {
    match tag {
        EnvTag::Local => "LOCAL",
        EnvTag::Development => "DEV",
        EnvTag::Staging => "STAGING",
        EnvTag::Production => "PROD",
    }
}

/// A tinted pill: text in `color` on a 14% fill of it (environment tags,
/// states). Caps, 10.5px semibold.
pub fn pill(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex()
        .items_center()
        .flex_none()
        .h(dp(18.))
        .px(dp(6.))
        .rounded(dp(5.))
        .bg(color.opacity(0.14))
        .text_color(color)
        .text_size(dp(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

/// The environment pill for a connection ("PROD").
pub fn env_pill(tag: EnvTag, cx: &App) -> Div {
    pill(env_short(tag), env_color(tag, cx))
}

/// A key cap ("esc", "↵"): bordered, 11px.
pub fn kbd(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = tokens(cx);
    div()
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .h(dp(18.))
        .min_w(dp(18.))
        .px(dp(5.))
        .rounded(dp(4.))
        .border_1()
        .border_color(t.border)
        .bg(t.surface)
        .text_size(dp(11.))
        .text_color(t.text_secondary)
        .child(text.into())
}

/// Label caps for section headers ("TABLES"): 11px semibold.
pub fn caps(text: &str, cx: &App) -> Div {
    div()
        .text_size(dp(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(tokens(cx).text_muted)
        .child(text.to_uppercase())
}

/// What the safe-mode badge says, its colour and icon (design.md, Review
/// questions #11): the effective behaviour, with "Auto ·" when it comes
/// from Automatic.
pub fn safe_mode_badge(cfg: &ConnectionConfig, cx: &App) -> (String, Hsla, Option<IconName>) {
    let t = tokens(cx);
    let effective = cfg.safety();
    let (label, color, icon) = match effective {
        SafeMode::Off | SafeMode::Auto => ("Safe mode off", t.text_muted, None),
        SafeMode::ConfirmWrites => ("Confirm writes", t.warning, None),
        SafeMode::ConfirmAll => ("Confirm every query", t.warning, Some(IconName::Shield)),
        SafeMode::ReadOnly => ("Read-only", t.danger, Some(IconName::Lock)),
    };
    let label = if cfg.safe_mode == SafeMode::Auto {
        match effective {
            SafeMode::Off | SafeMode::Auto => "Auto · Off".to_string(),
            _ => format!("Auto · {label}"),
        }
    } else {
        label.to_string()
    };
    (label, color, icon)
}

/// Enum pill colours (design.md, Review questions #10): never the state
/// colours (amber edited, red deleted, green new), so a value can't pass for
/// a staged change. A value always gets the same colour.
pub fn enum_color(value: &str, cx: &App) -> Hsla {
    let t = tokens(cx);
    let dark = cx.theme().mode.is_dark();
    let hex = |d: u32, l: u32| -> Hsla { gpui::rgb(if dark { d } else { l }).into() };
    let palette = [
        t.info,
        t.keyword,
        t.number,
        hex(0xF07AC0, 0xC2408F),
        hex(0x94A3B8, 0x64748B),
    ];
    // FNV-1a: stable across runs and platforms.
    let hash = value.bytes().fold(0x811c9dc5u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x01000193)
    });
    palette[hash as usize % palette.len()]
}

/// An engine's badge colour (design.md, screen 03): Postgres blue, MySQL
/// orange, SQLite teal, Redis red, MongoDB green.
pub fn engine_color(driver: Driver, cx: &App) -> Hsla {
    let t = tokens(cx);
    match driver {
        Driver::Postgres => t.info,
        Driver::MySql if cx.theme().mode.is_dark() => gpui::rgb(0xF29D4B).into(),
        Driver::MySql => gpui::rgb(0xC2762B).into(),
        Driver::Sqlite => t.number,
        Driver::Redis => t.danger,
        Driver::Mongo => t.success,
    }
}

/// The two letters on an engine badge.
pub fn engine_initials(driver: Driver) -> &'static str {
    match driver {
        Driver::Postgres => "PG",
        Driver::MySql => "MY",
        Driver::Sqlite => "SQ",
        Driver::Redis => "RD",
        Driver::Mongo => "MG",
    }
}

/// A square engine badge: initials on a tint of the engine colour.
pub fn engine_badge(driver: Driver, size: f32, cx: &App) -> Div {
    let color = engine_color(driver, cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(dp(size))
        .rounded(dp(size * 0.28))
        .bg(color.opacity(0.16))
        .text_color(color)
        .text_size(dp(size * 0.36))
        .font_weight(FontWeight::BOLD)
        .child(engine_initials(driver))
}

/// Where saved passwords live on this platform.
pub fn keychain_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS Keychain"
    } else if cfg!(windows) {
        "Windows Credential Manager"
    } else {
        "the Secret Service keyring"
    }
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
        // Panics on a missing or bad token.
        let _ = TOKEN_SETS.dark;
    }

    #[gpui::test]
    fn safe_mode_badge_says_what_happens(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            let t = tokens(cx);
            let badge = |tag, mode, cx: &App| {
                let mut cfg = ConnectionConfig::new(crate::db::Driver::Postgres);
                cfg.tag = tag;
                cfg.safe_mode = mode;
                let (label, color, _) = safe_mode_badge(&cfg, cx);
                (label, color)
            };
            assert_eq!(
                badge(EnvTag::Local, SafeMode::Auto, cx),
                ("Auto · Off".into(), t.text_muted)
            );
            assert_eq!(
                badge(EnvTag::Production, SafeMode::Auto, cx),
                ("Auto · Confirm writes".into(), t.warning)
            );
            assert_eq!(
                badge(EnvTag::Production, SafeMode::Off, cx),
                ("Safe mode off".into(), t.text_muted)
            );
            assert_eq!(
                badge(EnvTag::Local, SafeMode::ReadOnly, cx),
                ("Read-only".into(), t.danger)
            );
            assert_eq!(
                badge(EnvTag::Local, SafeMode::ConfirmAll, cx),
                ("Confirm every query".into(), t.warning)
            );
        });
    }
}
