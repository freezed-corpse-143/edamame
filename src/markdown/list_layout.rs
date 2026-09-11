//! Raw ↔ rendered column geometry of list-item marker prefixes, shared by the overlay painters
//! and the mouse click-to-offset mapping.
//!
//! The two widths differ whenever the bullet is substituted, an ordered marker is right-aligned in
//! a max-digit slot (`1. ` → ` 1. `), or the renderer's fixed child indent disagrees with the
//! source's.

/// Map a raw column on a list-item line to its rendered column; `None` when the line isn't a list
/// item, on which callers treat raw-col as visual-col.
///
/// Columns inside the raw marker collapse to the rendered content column.
pub fn list_raw_col_to_rendered_col(
    raw_text: &str,
    line: &ratatui::text::Line<'_>,
    raw_col: usize,
) -> Option<usize> {
    let raw_total = raw_list_marker_char_width(raw_text)?;
    let rendered_total = rendered_list_marker_char_width(line)?;
    if raw_col <= raw_total {
        Some(rendered_total)
    } else {
        Some(raw_col - raw_total + rendered_total)
    }
}

/// Inverse of [`list_raw_col_to_rendered_col`] over the marker cells.
///
/// Right-aligned on purpose: the 4-char task checkbox occupies the *last* four cells of both
/// markers, so aligning from the right keeps `[ ]` clicks exact even when the leading indent or
/// number padding differs.
pub fn list_rendered_col_to_raw_col_marker(
    raw_total: usize,
    rendered_total: usize,
    rendered_col: usize,
) -> usize {
    (rendered_col + raw_total)
        .saturating_sub(rendered_total)
        .min(raw_total)
}

/// Char width of the raw prefix: indent + marker + optional `[ ] ` task box.  `None` when
/// `raw_text` doesn't start with a list marker.
pub fn raw_list_marker_char_width(raw_text: &str) -> Option<usize> {
    let indent_chars = raw_text
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    let after_indent: String = raw_text.chars().skip(indent_chars).collect();
    let rb = after_indent.as_bytes();
    let marker_len = match rb.first() {
        Some(b'-') | Some(b'*') | Some(b'+') if rb.get(1) == Some(&b' ') => 2,
        _ => {
            let digits = rb.iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 0
                && matches!(rb.get(digits), Some(b'.') | Some(b')'))
                && rb.get(digits + 1) == Some(&b' ')
            {
                digits + 2
            } else {
                return None;
            }
        }
    };
    let after_marker = &after_indent[marker_len..];
    let task_len = if after_marker.starts_with("[ ] ")
        || after_marker.starts_with("[x] ")
        || after_marker.starts_with("[X] ")
    {
        4
    } else {
        0
    };
    Some(indent_chars + marker_len + task_len)
}

/// Char width of the rendered marker: indent, `• ` or padded digits, plus an optional `[ ] ` task
/// box.  `None` when the line doesn't start with a recognizable marker.
pub fn rendered_list_marker_char_width(line: &ratatui::text::Line<'_>) -> Option<usize> {
    let text: String = line.spans.iter().flat_map(|s| s.content.chars()).collect();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() && chars[i] == ' ' {
        i += 1;
    }
    let after_bullet = if chars.get(i) == Some(&'•') && chars.get(i + 1) == Some(&' ') {
        Some(i + 2)
    } else {
        let digits = chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0
            && matches!(chars.get(i + digits), Some('.') | Some(')'))
            && chars.get(i + digits + 1) == Some(&' ')
        {
            Some(i + digits + 2)
        } else {
            None
        }
    }?;
    // A task is a decorated bullet: include the checkbox's four cells so cursor and selection
    // mapping cover the whole marker.
    if chars.get(after_bullet) == Some(&'[')
        && matches!(chars.get(after_bullet + 1), Some(' ') | Some('✓'))
        && chars.get(after_bullet + 2) == Some(&']')
        && chars.get(after_bullet + 3) == Some(&' ')
    {
        Some(after_bullet + 4)
    } else {
        Some(after_bullet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::style::Style;
    use ratatui::text::{Line, Span};

    #[test]
    fn raw_list_marker_width_bullet() {
        assert_eq!(raw_list_marker_char_width("- foo"), Some(2));
        assert_eq!(raw_list_marker_char_width("  - foo"), Some(4));
    }

    #[test]
    fn raw_list_marker_width_ordered() {
        assert_eq!(raw_list_marker_char_width("1. foo"), Some(3));
        assert_eq!(raw_list_marker_char_width("10. foo"), Some(4));
    }

    #[test]
    fn raw_list_marker_width_task() {
        assert_eq!(raw_list_marker_char_width("- [ ] foo"), Some(6));
        assert_eq!(raw_list_marker_char_width("- [x] foo"), Some(6));
    }

    #[test]
    fn rendered_marker_width_bullet() {
        let line = Line::from(vec![Span::styled("• ", Style::default()), Span::raw("foo")]);
        assert_eq!(rendered_list_marker_char_width(&line), Some(2));
    }

    #[test]
    fn rendered_marker_width_ordered_padded() {
        let line = Line::from(vec![
            Span::styled(" 1. ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(rendered_list_marker_char_width(&line), Some(4));
    }

    #[test]
    fn rendered_marker_width_task() {
        // `• [ ] foo`: bullet + space + checkbox + space.
        let line = Line::from(vec![
            Span::styled("• ", Style::default()),
            Span::styled("[ ] ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(rendered_list_marker_char_width(&line), Some(6));
    }

    #[test]
    fn rendered_marker_width_task_checked() {
        let line = Line::from(vec![
            Span::styled("• ", Style::default()),
            Span::styled("[✓] ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(rendered_list_marker_char_width(&line), Some(6));
    }

    #[test]
    fn list_col_map_bullet_unchanged() {
        // Both markers are 2 chars wide.
        let line = Line::from(vec![Span::styled("• ", Style::default()), Span::raw("foo")]);
        assert_eq!(list_raw_col_to_rendered_col("- foo", &line, 2), Some(2));
        assert_eq!(list_raw_col_to_rendered_col("- foo", &line, 4), Some(4));
    }

    #[test]
    fn list_col_map_task_aligns_one_to_one() {
        // Both markers are 6 chars wide.
        let line = Line::from(vec![
            Span::styled("• ", Style::default()),
            Span::styled("[ ] ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(list_raw_col_to_rendered_col("- [ ] foo", &line, 6), Some(6));
        assert_eq!(list_raw_col_to_rendered_col("- [ ] foo", &line, 7), Some(7));
    }

    #[test]
    fn list_col_map_ordered_padded_shifts_right() {
        // Raw marker 3 chars, rendered 4.
        let line = Line::from(vec![
            Span::styled(" 1. ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(list_raw_col_to_rendered_col("1. foo", &line, 3), Some(4));
        assert_eq!(list_raw_col_to_rendered_col("1. foo", &line, 5), Some(6));
    }

    // ── Inverse marker map ────────────────────────────────────────────────

    #[test]
    fn inverse_marker_map_identity_when_widths_match() {
        for col in 0..6 {
            assert_eq!(list_rendered_col_to_raw_col_marker(6, 6, col), col);
        }
    }

    #[test]
    fn inverse_marker_map_right_aligns_padded_ordered() {
        // `1. ` (3) rendered as ` 1. ` (4): the pad cell maps to raw col 0, the rest shift left.
        assert_eq!(list_rendered_col_to_raw_col_marker(3, 4, 0), 0);
        assert_eq!(list_rendered_col_to_raw_col_marker(3, 4, 1), 0);
        assert_eq!(list_rendered_col_to_raw_col_marker(3, 4, 3), 2);
    }

    #[test]
    fn inverse_marker_map_right_aligns_nested_task_checkbox() {
        // `  - [ ] ` (8) vs `    • [ ] ` (10): the rendered `[` at col 6 is the raw `[` at 4.
        assert_eq!(list_rendered_col_to_raw_col_marker(8, 10, 6), 4);
        assert_eq!(list_rendered_col_to_raw_col_marker(8, 10, 8), 6);
    }

    #[test]
    fn inverse_marker_map_clamps_to_raw_total() {
        assert_eq!(list_rendered_col_to_raw_col_marker(3, 4, 10), 3);
    }

    /// Forward and inverse must agree at the marker boundary.
    #[test]
    fn inverse_marker_map_round_trips_boundary() {
        for (raw_text, rendered) in [
            ("- foo", "• foo"),
            ("1. foo", " 1. foo"),
            ("- [ ] foo", "• [ ] foo"),
            ("  - bar", "    • bar"),
        ] {
            let line = Line::from(vec![Span::raw(rendered.to_owned())]);
            let raw_total = raw_list_marker_char_width(raw_text).unwrap();
            let rendered_total = rendered_list_marker_char_width(&line).unwrap();
            assert_eq!(
                list_raw_col_to_rendered_col(raw_text, &line, raw_total),
                Some(rendered_total)
            );
            assert!(
                list_rendered_col_to_raw_col_marker(raw_total, rendered_total, rendered_total)
                    == raw_total
            );
        }
    }
}
