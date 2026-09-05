use std::fmt::Write;

use ratatui::{buffer::Buffer, layout::Rect, style::Style};

/// Gutter width: digits needed for `line_count` plus two columns of padding.
pub fn gutter_width(line_count: usize) -> u16 {
    if line_count == 0 {
        return 0;
    }
    digit_count(line_count) as u16 + 2
}

/// Split a document area into an optional gutter rect (left) and the content
/// rect.  Returns `(None, full)` when there is no gutter or no room for one.
pub fn split_gutter(full: Rect, line_count: usize) -> (Option<Rect>, Rect) {
    let gw = gutter_width(line_count);
    if gw == 0 || full.width <= gw {
        return (None, full);
    }
    let gutter = Rect {
        x: full.x,
        y: full.y,
        width: gw,
        height: full.height,
    };
    let content = Rect {
        x: full.x + gw,
        width: full.width - gw,
        ..full
    };
    (Some(gutter), content)
}

/// Paint right-aligned line numbers into `gutter_area`.
///
/// `line_at_visual_row(global_row, width)` maps a visual row (scroll + screen
/// row) to the 0-based source line to label it with, or `None` for rows with no
/// number (wrap continuations, table borders, image rows, past-EOF).  The caller
/// owns that mapping because only it knows its row coordinate space.
pub fn paint_gutter(
    buf: &mut Buffer,
    gutter_area: Rect,
    scroll: usize,
    line_count: usize,
    line_at_visual_row: impl Fn(usize, usize) -> Option<usize>,
    width: usize,
    style: Style,
) {
    if gutter_area.width == 0 || gutter_area.height == 0 {
        return;
    }
    let digit_w = digit_count(line_count);
    let mut num_buf = String::with_capacity(digit_w + 2);

    for vis_y in 0..gutter_area.height {
        let global_row = scroll + vis_y as usize;
        let line = line_at_visual_row(global_row, width);

        let y = gutter_area.y + vis_y;

        if let Some(line_idx) = line.filter(|&l| l < line_count) {
            num_buf.clear();
            let _ = write!(num_buf, "{:>w$}  ", line_idx + 1, w = digit_w);
            buf.set_string(gutter_area.x, y, &num_buf, style);
        } else {
            for x in gutter_area.x..gutter_area.x + gutter_area.width {
                let cell = &mut buf[(x, y)];
                cell.set_char(' ');
                cell.set_style(style);
            }
        }
    }
}

fn digit_count(n: usize) -> usize {
    n.checked_ilog10().map_or(1, |d| d as usize + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_count_edge_cases() {
        assert_eq!(digit_count(0), 1);
        assert_eq!(digit_count(1), 1);
        assert_eq!(digit_count(9), 1);
        assert_eq!(digit_count(10), 2);
        assert_eq!(digit_count(99), 2);
        assert_eq!(digit_count(100), 3);
        assert_eq!(digit_count(999), 3);
        assert_eq!(digit_count(1000), 4);
    }

    #[test]
    fn gutter_width_includes_padding() {
        assert_eq!(gutter_width(0), 0);
        assert_eq!(gutter_width(1), 3);
        assert_eq!(gutter_width(9), 3);
        assert_eq!(gutter_width(10), 4);
        assert_eq!(gutter_width(100), 5);
    }
}
