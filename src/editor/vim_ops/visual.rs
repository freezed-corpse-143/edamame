//! Visual-selection span widening — shared by the render path, the Visual-mode operators, and the
//! system `Copy`/`Cut` path so they can never disagree on what "the selection" means.
//!
//! **`Selection` stays half-open everywhere; vim's semantics are derived, never stored.**  Charwise
//! inclusivity ([`visual_charwise_range`]) and whole-line expansion ([`visual_line_char_range`]) are
//! computed on demand, which keeps a `v`↔`V` toggle lossless.  Don't "fix" it by snapping `active`:
//! that field is shared with the non-vim selection paths, which are genuinely half-open.

use std::ops::Range;

use crate::document::{next_grapheme_offset, Buffer, Selection};

/// Which flavor of Visual sub-mode a `selection` is read under, so the render path can pick the
/// matching widening without depending on `VimState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualKind {
    /// Charwise `v` — inclusive of the character under the cursor.
    Char,
    /// Linewise `V` — expanded to whole lines.
    Line,
}

/// The char range a charwise Visual `selection` covers: the stored half-open span with its *high*
/// end extended over the character it sits on, matching vim's inclusive Visual selection.
///
/// The extension is suppressed at a line end or end-of-buffer: edamame's vim cursor is an insertion
/// point that can sit on the newline slot (where `$` parks it), and the half-open span already
/// reaches past the last character there, so extending would swallow the newline.  That makes `v$`
/// and `v` + `l`-to-the-end agree.  Reverse selections need no special case — the rule applies to
/// whichever end is the high one.
pub fn visual_charwise_range(sel: &Selection, buf: &Buffer) -> Range<usize> {
    let len = buf.len_chars();
    let (lo, hi) = sel.range();
    let (lo, hi) = (lo.min(len), hi.min(len));
    let end = if hi < len && buf.rope().char(hi) != '\n' {
        next_grapheme_offset(buf, hi)
    } else {
        hi
    };
    lo..end
}

/// The char range `sel` covers under `kind` — the one dispatcher every consumer of a vim Visual
/// selection should call.  `None` yields the raw half-open span (the non-vim selection paths).
///
/// Every arm clamps to the buffer length: a non-vim selection is not cleared by a vim Normal-mode
/// edit, so an edit that shortens the document leaves `sel` pointing past the new end, and the
/// render overlays feed this range straight into `Rope::char_to_byte`, which panics out of bounds.
pub fn visual_span(sel: &Selection, buf: &Buffer, kind: Option<VisualKind>) -> Range<usize> {
    match kind {
        Some(VisualKind::Char) => visual_charwise_range(sel, buf),
        Some(VisualKind::Line) => visual_line_char_range(sel, buf),
        None => {
            let len = buf.len_chars();
            let (lo, hi) = sel.range();
            lo.min(len)..hi.min(len)
        }
    }
}

/// The inclusive buffer-line range a VisualLine `selection` covers: from the
/// line holding the selection start to the line holding its end.
pub fn visual_line_bounds(sel: &Selection, buf: &Buffer) -> (usize, usize) {
    let len = buf.len_chars();
    let (start, end) = sel.range();
    let first = buf.char_to_line(start.min(len));
    let last = buf.char_to_line(end.min(len));
    (first, last)
}

/// The char range a VisualLine `selection` expands to: the whole lines from
/// [`visual_line_bounds`], including the last line's trailing newline (or up to end-of-buffer).
pub fn visual_line_char_range(sel: &Selection, buf: &Buffer) -> Range<usize> {
    let (first, last) = visual_line_bounds(sel, buf);
    let line_count = buf.line_count();
    let start = buf.line_to_char(first);
    let end = if last + 1 < line_count {
        buf.line_to_char(last + 1)
    } else {
        buf.len_chars()
    };
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    fn sel(anchor: usize, active: usize) -> Selection {
        Selection { anchor, active }
    }

    #[test]
    fn bounds_span_the_touched_lines() {
        let b = buf("alpha\nbeta\ngamma\n");
        assert_eq!(visual_line_bounds(&sel(2, 13), &b), (0, 2));
        // Reversed (active before anchor) normalizes the same way.
        assert_eq!(visual_line_bounds(&sel(13, 2), &b), (0, 2));
    }

    #[test]
    fn char_range_covers_whole_lines_with_trailing_newline() {
        let b = buf("alpha\nbeta\ngamma\n");
        assert_eq!(visual_line_char_range(&sel(2, 7), &b), 0..11);
    }

    #[test]
    fn char_range_on_final_line_clamps_to_eof() {
        let b = buf("alpha\nbeta");
        assert_eq!(visual_line_char_range(&sel(7, 9), &b), 6..10);
    }

    #[test]
    fn charwise_range_includes_the_char_under_the_cursor() {
        let b = buf("abc\ndef");
        assert_eq!(visual_charwise_range(&sel(0, 0), &b), 0..1);
        assert_eq!(visual_charwise_range(&sel(0, 1), &b), 0..2);
    }

    #[test]
    fn charwise_range_stops_at_a_line_end() {
        let b = buf("abc\ndef");
        assert_eq!(visual_charwise_range(&sel(0, 2), &b), 0..3);
        // Cursor on the newline slot (where `$` parks it): no extension.
        assert_eq!(visual_charwise_range(&sel(0, 3), &b), 0..3);
    }

    #[test]
    fn charwise_range_at_end_of_buffer_does_not_overrun() {
        let b = buf("abc");
        assert_eq!(visual_charwise_range(&sel(0, 3), &b), 0..3);
        assert_eq!(visual_charwise_range(&sel(0, 9), &b), 0..3);
    }

    #[test]
    fn charwise_range_extends_the_high_end_of_a_reverse_selection() {
        let b = buf("abcd");
        // Anchor on 'c', cursor carried back to 'a': the anchor's char is the high end.
        assert_eq!(visual_charwise_range(&sel(2, 0), &b), 0..3);
    }

    #[test]
    fn charwise_range_extends_by_a_whole_grapheme() {
        // A combining sequence is one cursor step, so one selection step.
        let b = buf("e\u{301}x");
        assert_eq!(visual_charwise_range(&sel(0, 0), &b), 0..2);
    }

    #[test]
    fn visual_span_dispatches_on_kind() {
        let b = buf("alpha\nbeta\n");
        let s = sel(0, 2);
        assert_eq!(visual_span(&s, &b, Some(VisualKind::Char)), 0..3);
        assert_eq!(visual_span(&s, &b, Some(VisualKind::Line)), 0..6);
        assert_eq!(visual_span(&s, &b, None), 0..2);
    }

    #[test]
    fn visual_span_none_arm_clamps_a_stale_selection() {
        // Regression: a stale non-vim selection reaching past a shortened buffer used to panic
        // in `Rope::char_to_byte`; every arm must clamp, `None` too.
        let b = buf("abc"); // len_chars() == 3
        assert_eq!(visual_span(&sel(1, 4), &b, None), 1..3);
        assert_eq!(visual_span(&sel(9, 12), &b, None), 3..3);
    }
}
