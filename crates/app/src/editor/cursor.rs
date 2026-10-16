//! Converts iced's line/column cursor position into a byte offset into
//! the full document text, which is what `sql_intel::complete` expects
//! (it only has to reason about one flat string, not iced's line model).

use iced::widget::text_editor::Content;

pub fn byte_offset(content: &Content) -> usize {
    let cursor = content.cursor();
    let position = cursor.position;

    let mut offset = 0;
    for i in 0..position.line {
        let Some(line) = content.line(i) else { break };
        offset += line.text.len() + line.ending.as_str().len();
    }

    if let Some(line) = content.line(position.line) {
        let byte_in_line = line.text.char_indices().nth(position.column).map(|(b, _)| b).unwrap_or(line.text.len());
        offset += byte_in_line;
    }

    offset
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::text_editor::{Action, Motion};

    #[test]
    fn byte_offset_tracks_cursor_across_multiple_lines() {
        let mut content = Content::with_text("SELECT *\nFROM users");
        // Move to the very end of the buffer.
        content.perform(Action::Move(Motion::DocumentEnd));
        assert_eq!(byte_offset(&content), "SELECT *\nFROM users".len());
    }

    #[test]
    fn byte_offset_is_zero_at_the_start_of_an_empty_document() {
        let content = Content::new();
        assert_eq!(byte_offset(&content), 0);
    }
}
