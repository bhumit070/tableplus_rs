//! Wires `sql-intel`'s dialect-aware tokenizer into iced's per-line
//! `Highlighter` trait, so the query editor's syntax colors update live
//! and automatically match whichever backend (Postgres/MySQL/MSSQL) the
//! active connection uses.

use db_core::connection::DriverKind;
use iced::advanced::text::Highlighter;
use iced::Color;
use sql_intel::TokenKind;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub dialect: Option<DriverKind>,
}

pub struct SqlHighlighter {
    dialect: Option<DriverKind>,
    current_line: usize,
}

impl Highlighter for SqlHighlighter {
    type Settings = Settings;
    type Highlight = TokenKind;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, TokenKind)>;

    fn new(settings: &Self::Settings) -> Self {
        SqlHighlighter { dialect: settings.dialect, current_line: 0 }
    }

    fn update(&mut self, new_settings: &Self::Settings) {
        self.dialect = new_settings.dialect;
    }

    fn change_line(&mut self, line: usize) {
        self.current_line = self.current_line.min(line);
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        self.current_line += 1;

        let dialect = self.dialect.map(sql_intel::dialect_for).unwrap_or_else(sql_intel::generic_dialect);
        let spans: Vec<(Range<usize>, TokenKind)> =
            sql_intel::highlight(line, dialect.as_ref()).into_iter().map(|s| (s.start..s.end, s.kind)).collect();
        spans.into_iter()
    }

    fn current_line(&self) -> usize {
        self.current_line
    }
}

/// Maps a highlighted token category to a display color. A fixed,
/// readable palette rather than a pluggable theme for now — see
/// DEVELOPMENT.md's roadmap for light/dark theme-aware editor colors.
pub fn color_for(kind: &TokenKind) -> Color {
    match kind {
        TokenKind::Keyword => Color::from_rgb8(0xC5, 0x86, 0xC0),
        TokenKind::Identifier => Color::from_rgb8(0xDC, 0xDC, 0xDC),
        TokenKind::StringLiteral => Color::from_rgb8(0xCE, 0x91, 0x78),
        TokenKind::NumberLiteral => Color::from_rgb8(0xB5, 0xCE, 0xA8),
        TokenKind::Operator => Color::from_rgb8(0xD4, 0xD4, 0xD4),
        TokenKind::Punctuation => Color::from_rgb8(0x80, 0x80, 0x80),
        TokenKind::Comment => Color::from_rgb8(0x6A, 0x99, 0x55),
        TokenKind::Whitespace => Color::from_rgb8(0xDC, 0xDC, 0xDC),
    }
}
