//! Vim search-query construction: [`word_under_cursor_at`] extracts the keyword for
//! `*` / `#`.  `vim_feed` turns it into a literal query for the base search feature
//! (smartcase lives there, not here).

use crate::document::Buffer;

use super::motion::{class, Class};

/// The keyword under the cursor — or, if the cursor is not on one, the next keyword on
/// the same line — for `*` / `#`.  Returns its **start** char offset and literal text,
/// or `None` when the line has no keyword at or after the cursor.
///
/// A keyword is a run of [`Class::Word`] chars; the scan never crosses a newline.  No
/// `\<word\>` boundaries are added — the base search is literal-substring, not regex.
/// Callers reposition the cursor to the returned start before searching (vim's behavior),
/// so a backward `#` from mid-word lands on the *previous* occurrence.
pub fn word_under_cursor_at(buf: &Buffer, cursor: usize) -> Option<(usize, String)> {
    let len = buf.len_chars();
    if len == 0 {
        return None;
    }
    let rope = buf.rope();
    let line = buf.char_to_line(cursor.min(len.saturating_sub(1)));
    let line_start = buf.line_to_char(line);
    let mut line_end = line_start;
    while line_end < len && rope.char(line_end) != '\n' {
        line_end += 1;
    }

    let mut pos = cursor.clamp(line_start, line_end);
    while pos < line_end && class(rope.char(pos), false) != Class::Word {
        pos += 1;
    }
    if pos >= line_end {
        return None;
    }

    let mut start = pos;
    while start > line_start && class(rope.char(start - 1), false) == Class::Word {
        start -= 1;
    }
    let mut end = pos;
    while end < line_end && class(rope.char(end), false) == Class::Word {
        end += 1;
    }
    Some((start, buf.slice_to_string(start, end)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    fn word(b: &Buffer, cursor: usize) -> Option<String> {
        word_under_cursor_at(b, cursor).map(|(_, t)| t)
    }

    #[test]
    fn word_under_cursor_returns_the_keyword_the_cursor_is_in() {
        let b = buf("foo bar baz");
        assert_eq!(word(&b, 0).as_deref(), Some("foo"));
        assert_eq!(word(&b, 5).as_deref(), Some("bar"));
        assert_eq!(word(&b, 10).as_deref(), Some("baz"));
    }

    #[test]
    fn word_under_cursor_reports_the_word_start_offset() {
        let b = buf("foo bar baz");
        // Start is 4, not 5 — what makes `#` jump to the previous occurrence.
        assert_eq!(word_under_cursor_at(&b, 5), Some((4, "bar".to_owned())));
        assert_eq!(word_under_cursor_at(&b, 3), Some((4, "bar".to_owned())));
    }

    #[test]
    fn word_under_cursor_skips_forward_to_the_next_keyword() {
        let b = buf("a   word");
        assert_eq!(word(&b, 1).as_deref(), Some("word"));
    }

    #[test]
    fn word_under_cursor_does_not_cross_a_newline() {
        let b = buf("end\nnext");
        assert_eq!(word(&b, 3), None);
    }

    #[test]
    fn word_under_cursor_includes_underscores_and_digits() {
        let b = buf("call foo_bar2 now");
        assert_eq!(word(&b, 5).as_deref(), Some("foo_bar2"));
    }

    #[test]
    fn word_under_cursor_is_none_on_empty_buffer() {
        assert_eq!(word_under_cursor_at(&buf(""), 0), None);
    }
}
