//! Shared text-field cursor helper for editor views and modal inputs.
//!
//! Every cursor is *fake* (a styled cell, never the hardware cursor): the hardware cursor shows
//! one position and its color can't be set portably (OSC 12 is unsupported by kitty and
//! mis-restores in VTE).  The cursor is always a block; context is signaled by color
//! (see `docs/dev/theming.md`).
//!
//! Two block mechanisms, not interchangeable:
//! 1. **Recolor-the-cell** ([`text_field_spans`]), preferred: the glyph under the cursor is
//!    restyled, always one cell wide, so the field never jitters on blink.
//! 2. **Insert-a-glyph** ([`CURSOR_BLOCK`]), fallback for rows whose cell styling is owned by a
//!    shared formatter or scroll window (`settings_overlay`, `export_theme_modal`).  Only ever
//!    placed at an append-only end-of-value position; the caller MUST emit a same-width space
//!    on the hidden blink phase.  Don't move those sites onto mechanism 1 without first giving
//!    them per-cell styling control.

use ratatui::style::Style;
use ratatui::text::Span;

/// Full-cell block glyph for mechanism 2 (see the module doc).  Prefer [`text_field_spans`].
pub const CURSOR_BLOCK: char = '█';

/// The three spans of a single-line field value with a blink-stable block cursor at char index
/// `cursor`.  The middle span is always one cell (the char under the cursor, or a space past
/// the end), styled `cursor_style` when `visible` and `value_style` otherwise.
pub fn text_field_spans(
    value: &str,
    cursor: usize,
    visible: bool,
    value_style: Style,
    cursor_style: Style,
) -> [Span<'static>; 3] {
    let (pre, rest) = split_at_char(value, cursor);
    let mut rest_chars = rest.chars();
    let under = rest_chars.next();
    let post: String = rest_chars.collect();
    let cell_style = if visible { cursor_style } else { value_style };
    [
        Span::styled(pre, value_style),
        Span::styled(under.unwrap_or(' ').to_string(), cell_style),
        Span::styled(post, value_style),
    ]
}

/// Split `s` at char index `cursor` (clamped to the end) into two owned halves.
fn split_at_char(s: &str, cursor: usize) -> (String, String) {
    let byte_idx = s
        .char_indices()
        .nth(cursor)
        .map(|(b, _)| b)
        .unwrap_or(s.len());
    (s[..byte_idx].to_owned(), s[byte_idx..].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_at_char_mid_and_past_end() {
        assert_eq!(split_at_char("hello", 2), ("he".into(), "llo".into()));
        assert_eq!(split_at_char("hi", 5), ("hi".into(), String::new()));
    }

    #[test]
    fn split_at_char_respects_char_boundaries() {
        assert_eq!(split_at_char("é!", 1), ("é".into(), "!".into()));
    }

    #[test]
    fn text_field_slot_is_constant_width_across_blink() {
        let vis = text_field_spans("note", 2, true, Style::default(), Style::default());
        let hid = text_field_spans("note", 2, false, Style::default(), Style::default());
        let width = |spans: &[Span<'static>]| -> usize {
            spans.iter().map(|s| s.content.chars().count()).sum()
        };
        assert_eq!(width(&vis), width(&hid));
        assert_eq!(vis[1].content.as_ref(), "t");
        assert_eq!(hid[1].content.as_ref(), "t");
    }

    #[test]
    fn text_field_cursor_past_end_is_a_space_cell() {
        let spans = text_field_spans("hi", 2, true, Style::default(), Style::default());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hi ");
        assert_eq!(spans[1].content.as_ref(), " ");
        assert!(spans[2].content.is_empty());
    }
}
