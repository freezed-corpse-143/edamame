//! Halfblocks partial-render helper. Native protocols (Sixel, Kitty, iTerm2) must re-encode
//! the whole image to clip it, which lags on every scrolled frame; halfblocks cells are
//! position-independent, so a pre-rendered scratch `Buffer` (built once by `ImageCache`) can
//! be cell-copied into the visible rows with no encoding work per frame.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

/// Copy a pre-rendered halfblocks `scratch` (sized to `full_rect`, origin `(0, 0)`) into
/// `buf` at `dst_rect`, starting `src_y_offset` image rows down. Cells outside `buf` or past
/// the image's bottom are silently dropped — the latter is exactly what an image scrolling
/// off the bottom wants.
///
/// `bg` replaces `Color::Reset` backgrounds: ratatui_image leaves `Reset` on letter-box
/// cells and transparent pixels, which would otherwise punch through to the terminal's own
/// background as dark bands around any partially visible image.
pub fn paint_halfblocks_partial(
    scratch: &Buffer,
    full_rect: Rect,
    src_y_offset: u16,
    dst_rect: Rect,
    buf: &mut Buffer,
    bg: Color,
) {
    if full_rect.width == 0 || full_rect.height == 0 || dst_rect.width == 0 || dst_rect.height == 0
    {
        return;
    }

    for dy in 0..dst_rect.height {
        let src_y = src_y_offset.saturating_add(dy);
        if src_y >= full_rect.height {
            break;
        }
        for dx in 0..dst_rect.width {
            if dx >= full_rect.width {
                break;
            }
            let Some(src_cell) = scratch.cell((dx, src_y)) else {
                continue;
            };
            let mut copied = src_cell.clone();
            // fg is the `▀` glyph's top-pixel color and must be preserved.
            if copied.bg == Color::Reset {
                copied.bg = bg;
            }
            if let Some(dst_cell) = buf.cell_mut((dst_rect.x + dx, dst_rect.y + dy)) {
                *dst_cell = copied;
            }
        }
    }
}

#[cfg(test)]
// `Picker::halfblocks()` hardcodes a font size that changes the cell counts these tests pin.
#[allow(deprecated)]
mod tests {
    use super::*;

    use image::DynamicImage;
    use ratatui::widgets::StatefulWidget;
    use ratatui_image::picker::Picker;
    use ratatui_image::{Resize, StatefulImage};

    /// A halfblocks scratch buffer built the way `ImageCache` does. The protocol is forced:
    /// `from_fontsize` infers it from `$TERM_PROGRAM`, and an iTerm2 picker would make these
    /// tests pass vacuously on a buffer of `skip` cells.
    fn halfblocks_scratch(rect: Rect) -> Buffer {
        let mut picker = Picker::from_fontsize((1, 2).into());
        picker.set_protocol_type(ratatui_image::picker::ProtocolType::Halfblocks);
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            u32::from(rect.width) * 2,
            u32::from(rect.height) * 4,
            image::Rgba([40, 80, 120, 255]),
        ));
        let mut protocol = picker.new_resize_protocol(img);
        let mut buf = Buffer::empty(rect);
        StatefulImage::default()
            .resize(Resize::Fit(None))
            .render(rect, &mut buf, &mut protocol);
        buf
    }

    #[test]
    fn positive_src_offset_shifts_source_rows() {
        let full = Rect::new(0, 0, 8, 6);
        let scratch = halfblocks_scratch(full);

        let mut dst_buf = Buffer::empty(Rect::new(0, 0, 8, 3));
        paint_halfblocks_partial(
            &scratch,
            full,
            2,
            Rect::new(0, 0, 8, 3),
            &mut dst_buf,
            Color::Reset,
        );
        for dy in 0..3u16 {
            for dx in 0..8u16 {
                let dst_cell = dst_buf.cell((dx, dy)).unwrap();
                let src_cell = scratch.cell((dx, 2 + dy)).unwrap();
                assert_eq!(dst_cell.symbol(), src_cell.symbol());
                assert_eq!(dst_cell.style(), src_cell.style());
            }
        }
    }

    #[test]
    fn destination_clipping_silently_drops_out_of_bounds_cells() {
        let full = Rect::new(0, 0, 8, 4);
        let scratch = halfblocks_scratch(full);
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        paint_halfblocks_partial(
            &scratch,
            full,
            0,
            Rect::new(0, 0, 6, 3),
            &mut buf,
            Color::Reset,
        );
    }

    #[test]
    fn src_offset_past_image_leaves_dst_default() {
        let full = Rect::new(0, 0, 8, 4);
        let scratch = halfblocks_scratch(full);
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 6));
        paint_halfblocks_partial(
            &scratch,
            full,
            5,
            Rect::new(0, 0, 8, 6),
            &mut buf,
            Color::Reset,
        );
        for y in 0..6u16 {
            for x in 0..8u16 {
                let c = buf.cell((x, y)).unwrap();
                assert_eq!(c.symbol(), " ");
            }
        }
    }

    #[test]
    fn zero_area_is_noop() {
        let scratch = halfblocks_scratch(Rect::new(0, 0, 4, 4));
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 4));
        paint_halfblocks_partial(
            &scratch,
            Rect::new(0, 0, 0, 4),
            0,
            Rect::new(0, 0, 4, 4),
            &mut buf,
            Color::Reset,
        );
        paint_halfblocks_partial(
            &scratch,
            Rect::new(0, 0, 4, 4),
            0,
            Rect::new(0, 0, 0, 4),
            &mut buf,
            Color::Reset,
        );
        for y in 0..4u16 {
            for x in 0..4u16 {
                assert_eq!(buf.cell((x, y)).unwrap().symbol(), " ");
            }
        }
    }
}
