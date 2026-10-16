//! Auto-closes brackets and quotes as you type them, the way every
//! mainstream code editor does: typing `(`, `'`, `"`, `` ` ``, or `[`
//! inserts the matching closer immediately after and leaves the cursor
//! between the pair.
//!
//! This only handles the "insert a pair" half of the usual UX; "typing
//! the closing character when one is already right there just moves past
//! it instead of inserting a duplicate" needs read access to the
//! surrounding text that iced's `key_binding` closure doesn't hand us
//! (see DEVELOPMENT.md's editor roadmap).

use iced::widget::text_editor::{Binding, KeyPress, Motion};

pub fn key_binding<Message>(key_press: KeyPress) -> Option<Binding<Message>> {
    let default = Binding::from_key_press(key_press)?;

    if let Binding::Insert(open) = default {
        if let Some(close) = matching_close(open) {
            return Some(Binding::Sequence(vec![
                Binding::Insert(open),
                Binding::Insert(close),
                Binding::Move(Motion::Left),
            ]));
        }
    }

    Some(default)
}

fn matching_close(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '\'' => Some('\''),
        '"' => Some('"'),
        '`' => Some('`'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_every_supported_opener_with_its_closer() {
        assert_eq!(matching_close('('), Some(')'));
        assert_eq!(matching_close('['), Some(']'));
        assert_eq!(matching_close('{'), Some('}'));
        assert_eq!(matching_close('\''), Some('\''));
        assert_eq!(matching_close('"'), Some('"'));
        assert_eq!(matching_close('`'), Some('`'));
    }

    #[test]
    fn leaves_non_pairable_characters_alone() {
        assert_eq!(matching_close('a'), None);
        assert_eq!(matching_close(';'), None);
    }
}
