//! Reusable vertical scrollbar.  The pure helpers [`thumb_range`], [`position_for_click`], and
//! [`position_for_drag`] are shared by the renderer and the App-layer mouse handler so both use
//! the same arithmetic.  Track (`│`) and thumb (`█`) glyphs differ in shape so monochrome
//! terminals can still tell them apart.

use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

use crate::config::Theme;
use crate::ui::scroll_container::ScrollContainerState;

/// Thumb height floor, in cells; keeps the thumb visible and draggable on long documents.
pub const MIN_THUMB: u16 = 2;

/// A rendered scrollbar's rect plus the numbers that produced it.  Published by
/// [`crate::ui::EditorViewState`] each frame so the App's mouse layer can hit-test the gutter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarMetrics {
    pub area: Rect,
    /// Total content rows, post-wrap.
    pub total: u16,
    /// Viewport height in rows.
    pub visible: u16,
    pub position: u16,
}

/// `(top_offset, height)` of the thumb within a `track`-row track, or `None` when the content
/// fits.  Height is proportional to `visible / total`, floored at [`MIN_THUMB`].
pub fn thumb_range(total: u16, visible: u16, position: u16, track: u16) -> Option<(u16, u16)> {
    if track == 0 || total == 0 || total <= visible {
        return None;
    }
    let total_u = total as u32;
    let visible_u = visible as u32;
    let track_u = track as u32;
    let raw_height = (visible_u * track_u + total_u / 2) / total_u;
    let thumb_h = (raw_height as u16).max(MIN_THUMB).min(track);
    let max_pos = total.saturating_sub(visible) as u32;
    let max_top = (track - thumb_h) as u32;
    let pos = (position as u32).min(max_pos);
    let top = if max_pos == 0 || max_top == 0 {
        0
    } else {
        ((pos * max_top) + (max_pos / 2)) / max_pos
    } as u16;
    Some((top, thumb_h))
}

/// Scroll position that centers the thumb on `click_row`, clamped.
pub fn position_for_click(total: u16, visible: u16, track: u16, click_row: u16) -> u16 {
    let Some((_, thumb_h)) = thumb_range(total, visible, 0, track) else {
        return 0;
    };
    let max_top = track.saturating_sub(thumb_h);
    if max_top == 0 {
        return 0;
    }
    let target_top = click_row.saturating_sub(thumb_h / 2).min(max_top);
    let max_pos = total.saturating_sub(visible) as u32;
    ((target_top as u32 * max_pos + (max_top as u32 / 2)) / max_top as u32) as u16
}

/// Scroll position for a drag: `grab_offset` is the row within the thumb where it was grabbed.
pub fn position_for_drag(
    total: u16,
    visible: u16,
    track: u16,
    pointer_row: u16,
    grab_offset: u16,
) -> u16 {
    let Some((_, thumb_h)) = thumb_range(total, visible, 0, track) else {
        return 0;
    };
    let max_top = track.saturating_sub(thumb_h);
    if max_top == 0 {
        return 0;
    }
    let target_top = pointer_row.saturating_sub(grab_offset).min(max_top);
    let max_pos = total.saturating_sub(visible) as u32;
    ((target_top as u32 * max_pos + (max_top as u32 / 2)) / max_top as u32) as u16
}

/// Render a [`Scrollbar`] from a [`ScrollContainerState`]; call only after `observe` so
/// `last_total` / `last_visible` reflect the current layout.
pub fn render_for_scroll_state(
    bar_area: Rect,
    state: &ScrollContainerState,
    theme: &Theme,
    buf: &mut Buffer,
) {
    let metrics = ScrollbarMetrics {
        area: bar_area,
        total: state.last_total,
        visible: state.last_visible,
        position: state.scroll,
    };
    Scrollbar {
        metrics,
        theme,
        active: false,
    }
    .render(bar_area, buf);
}

/// Vertical scrollbar widget; `active` selects the bright thumb style used while hovering or
/// dragging.
pub struct Scrollbar<'a> {
    pub metrics: ScrollbarMetrics,
    pub theme: &'a Theme,
    pub active: bool,
}

impl<'a> Widget for Scrollbar<'a> {
    fn render(self, _area: Rect, buf: &mut Buffer) {
        let area = self.metrics.area;
        if area.width == 0 || area.height == 0 {
            return;
        }
        let track = area.height;
        let track_style = self.theme.scrollbar_track;
        for y in 0..track {
            let cell = &mut buf[(area.x, area.y + y)];
            cell.set_symbol("│").set_style(track_style);
        }
        if let Some((top, thumb_h)) = thumb_range(
            self.metrics.total,
            self.metrics.visible,
            self.metrics.position,
            track,
        ) {
            let thumb_style = if self.active {
                self.theme.scrollbar_thumb_active
            } else {
                self.theme.scrollbar_thumb
            };
            for y in top..(top + thumb_h).min(track) {
                let cell = &mut buf[(area.x, area.y + y)];
                cell.set_symbol("█").set_style(thumb_style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_thumb_when_content_fits() {
        assert_eq!(thumb_range(10, 10, 0, 20), None);
        assert_eq!(thumb_range(5, 10, 0, 20), None);
        assert_eq!(thumb_range(0, 0, 0, 20), None);
    }

    #[test]
    fn no_thumb_with_zero_track() {
        assert_eq!(thumb_range(100, 10, 0, 0), None);
    }

    #[test]
    fn thumb_at_top_when_position_zero() {
        let (top, _) = thumb_range(100, 10, 0, 20).unwrap();
        assert_eq!(top, 0);
    }

    #[test]
    fn thumb_at_bottom_when_position_max() {
        let (top, h) = thumb_range(100, 10, 90, 20).unwrap();
        assert_eq!(top + h, 20);
    }

    #[test]
    fn thumb_height_scales_with_visible_over_total() {
        let (_, h) = thumb_range(100, 50, 0, 20).unwrap();
        assert_eq!(h, 10);
    }

    #[test]
    fn thumb_floored_at_minimum() {
        let (_, h) = thumb_range(10000, 1, 0, 20).unwrap();
        assert!(h >= MIN_THUMB);
    }

    #[test]
    fn thumb_height_capped_at_track() {
        let (top, h) = thumb_range(11, 10, 0, 5).unwrap();
        assert!(h <= 5);
        assert_eq!(top, 0);
    }

    #[test]
    fn position_for_click_centres_thumb() {
        // thumb_h = 2, max_top = 18; click at row 9 → top 8 → scroll ≈ 8/18 * 90 = 40.
        let p = position_for_click(100, 10, 20, 9);
        assert!((38..=42).contains(&p), "got {p}");
    }

    #[test]
    fn position_for_click_clamps_within_track() {
        let p = position_for_click(100, 10, 20, 100);
        assert_eq!(p, 90);
        let p = position_for_click(100, 10, 20, 0);
        assert_eq!(p, 0);
    }

    #[test]
    fn position_for_drag_uses_grab_offset() {
        let p = position_for_drag(100, 10, 20, 9, 1);
        assert!((38..=42).contains(&p), "got {p}");
    }

    #[test]
    fn position_for_click_with_no_overflow_returns_zero() {
        assert_eq!(position_for_click(5, 10, 20, 5), 0);
    }
}
