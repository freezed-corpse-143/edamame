//! Editor-area dimming behind a modal.
//!
//! Truecolor terminals ([`ColorDepth::TrueColor`]) blend each cell's fg/bg toward the theme's
//! `default_bg` by [`BLEND_T`], which keeps document structure legible as silhouettes.  Anything
//! else gets a [`Modifier::DIM`] sweep; Ansi256 also forces the foreground to `text_muted`,
//! because terminals often render `DIM` as a barely visible ~10% luminance drop.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use crate::config::Theme;
use crate::terminal::{Capabilities, ColorDepth};

/// Blend fraction toward `default_bg` on truecolor terminals: 0.0 = untouched, 1.0 = erased.
const BLEND_T: f32 = 0.6;

/// Dim `area` of `buf` using the strategy for the active color depth.
pub fn dim_area(buf: &mut Buffer, area: Rect, caps: &Capabilities, theme: &Theme) {
    match caps.color_depth {
        ColorDepth::TrueColor => dim_truecolor(buf, area, theme),
        ColorDepth::Ansi256 => dim_ansi256(buf, area, theme),
        ColorDepth::Ansi16 | ColorDepth::NoColor => dim_modifier_only(buf, area),
    }
}

/// Truecolor sweep.  A `Color::Reset` side is left untouched: the terminal's real default
/// color is unknown.
fn dim_truecolor(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let target = match color_to_rgb(theme.default_bg()) {
        Some(rgb) => rgb,
        None => return dim_modifier_only(buf, area),
    };
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            let Some(cell) = buf.cell_mut((x, y)) else {
                continue;
            };
            if let Some(fg) = color_to_rgb(cell.fg) {
                let blended = blend_toward(fg, target, BLEND_T);
                cell.fg = Color::Rgb(blended[0], blended[1], blended[2]);
            }
            if let Some(bg) = color_to_rgb(cell.bg) {
                let blended = blend_toward(bg, target, BLEND_T);
                cell.bg = Color::Rgb(blended[0], blended[1], blended[2]);
            }
        }
    }
}

/// Ansi256 sweep: `text_muted` foreground plus `Modifier::DIM`.
fn dim_ansi256(buf: &mut Buffer, area: Rect, theme: &Theme) {
    let muted = theme.text_muted();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.fg = muted;
                cell.modifier.insert(Modifier::DIM);
            }
        }
    }
}

/// Plain `Modifier::DIM` sweep for low-color or monochrome terminals.
fn dim_modifier_only(buf: &mut Buffer, area: Rect) {
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.modifier.insert(Modifier::DIM);
            }
        }
    }
}

/// Linear blend from `src` toward `target` by `t` ∈ `[0, 1]`.
fn blend_toward(src: [u8; 3], target: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| -> u8 {
        let af = a as f32;
        let bf = b as f32;
        (af + (bf - af) * t).round().clamp(0.0, 255.0) as u8
    };
    [
        mix(src[0], target[0]),
        mix(src[1], target[1]),
        mix(src[2], target[2]),
    ]
}

/// RGB triple for a `Color`; `None` for `Color::Reset`, whose concrete RGB is unknown.
fn color_to_rgb(color: Color) -> Option<[u8; 3]> {
    match color {
        Color::Reset => None,
        Color::Black => Some(ANSI_PALETTE[0]),
        Color::Red => Some(ANSI_PALETTE[1]),
        Color::Green => Some(ANSI_PALETTE[2]),
        Color::Yellow => Some(ANSI_PALETTE[3]),
        Color::Blue => Some(ANSI_PALETTE[4]),
        Color::Magenta => Some(ANSI_PALETTE[5]),
        Color::Cyan => Some(ANSI_PALETTE[6]),
        Color::Gray => Some(ANSI_PALETTE[7]),
        Color::DarkGray => Some(ANSI_PALETTE[8]),
        Color::LightRed => Some(ANSI_PALETTE[9]),
        Color::LightGreen => Some(ANSI_PALETTE[10]),
        Color::LightYellow => Some(ANSI_PALETTE[11]),
        Color::LightBlue => Some(ANSI_PALETTE[12]),
        Color::LightMagenta => Some(ANSI_PALETTE[13]),
        Color::LightCyan => Some(ANSI_PALETTE[14]),
        Color::White => Some(ANSI_PALETTE[15]),
        Color::Rgb(r, g, b) => Some([r, g, b]),
        Color::Indexed(i) => Some(ANSI_PALETTE[i as usize]),
    }
}

/// Standard xterm 256-color palette: 16 system colors (the conventional defaults, since the
/// terminal's own are unknowable), the 6×6×6 cube, then the 24-step gray ramp.
const ANSI_PALETTE: [[u8; 3]; 256] = build_ansi_palette();

const fn build_ansi_palette() -> [[u8; 3]; 256] {
    let mut p = [[0u8; 3]; 256];
    p[0] = [0, 0, 0];
    p[1] = [128, 0, 0];
    p[2] = [0, 128, 0];
    p[3] = [128, 128, 0];
    p[4] = [0, 0, 128];
    p[5] = [128, 0, 128];
    p[6] = [0, 128, 128];
    p[7] = [192, 192, 192];
    p[8] = [128, 128, 128];
    p[9] = [255, 0, 0];
    p[10] = [0, 255, 0];
    p[11] = [255, 255, 0];
    p[12] = [0, 0, 255];
    p[13] = [255, 0, 255];
    p[14] = [0, 255, 255];
    p[15] = [255, 255, 255];
    let levels: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let mut r = 0;
    while r < 6 {
        let mut g = 0;
        while g < 6 {
            let mut b = 0;
            while b < 6 {
                let i = 16 + 36 * r + 6 * g + b;
                p[i] = [levels[r], levels[g], levels[b]];
                b += 1;
            }
            g += 1;
        }
        r += 1;
    }
    let mut k = 0;
    while k < 24 {
        let v = 8 + 10 * k as u8;
        p[232 + k] = [v, v, v];
        k += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_at_zero_is_identity() {
        assert_eq!(blend_toward([100, 50, 200], [0, 0, 0], 0.0), [100, 50, 200]);
    }

    #[test]
    fn blend_at_one_is_target() {
        assert_eq!(
            blend_toward([100, 50, 200], [33, 44, 55], 1.0),
            [33, 44, 55]
        );
    }

    #[test]
    fn blend_halfway_is_midpoint() {
        let r = blend_toward([100, 50, 200], [0, 100, 0], 0.5);
        assert_eq!(r, [50, 75, 100]);
    }

    #[test]
    fn blend_clamps_t_below_zero() {
        assert_eq!(
            blend_toward([100, 50, 200], [0, 0, 0], -1.0),
            [100, 50, 200]
        );
    }

    #[test]
    fn blend_clamps_t_above_one() {
        assert_eq!(
            blend_toward([100, 50, 200], [33, 44, 55], 2.0),
            [33, 44, 55]
        );
    }

    #[test]
    fn ansi_palette_known_indices() {
        assert_eq!(ANSI_PALETTE[0], [0, 0, 0]); // black
        assert_eq!(ANSI_PALETTE[15], [255, 255, 255]); // white
        assert_eq!(ANSI_PALETTE[16], [0, 0, 0]); // cube 0,0,0
        assert_eq!(ANSI_PALETTE[231], [255, 255, 255]); // cube 5,5,5
        assert_eq!(ANSI_PALETTE[232], [8, 8, 8]); // first grey
        assert_eq!(ANSI_PALETTE[255], [238, 238, 238]); // last grey
        assert_eq!(ANSI_PALETTE[196], [255, 0, 0]); // bright red
        assert_eq!(ANSI_PALETTE[208], [255, 135, 0]); // orange
    }

    #[test]
    fn color_to_rgb_handles_indexed_and_rgb() {
        assert_eq!(color_to_rgb(Color::Reset), None);
        assert_eq!(color_to_rgb(Color::Indexed(196)), Some([255, 0, 0]));
        assert_eq!(color_to_rgb(Color::Rgb(10, 20, 30)), Some([10, 20, 30]));
        assert_eq!(color_to_rgb(Color::Red), Some(ANSI_PALETTE[1]));
    }

    #[test]
    fn dim_truecolor_blends_each_cell_toward_default_bg() {
        use ratatui::style::Style;
        let theme = crate::config::Theme::default();
        let target = color_to_rgb(theme.default_bg()).expect("default theme has rgb-able bg");
        let expected_fg = blend_toward([200, 100, 0], target, BLEND_T);
        let expected_bg = blend_toward([20, 20, 20], target, BLEND_T);

        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::empty(area);
        for x in 0..4 {
            buf.cell_mut((x, 0)).unwrap().set_style(
                Style::default()
                    .fg(Color::Rgb(200, 100, 0))
                    .bg(Color::Rgb(20, 20, 20)),
            );
        }

        dim_truecolor(&mut buf, area, &theme);

        for x in 0..4 {
            let cell = buf.cell((x, 0)).unwrap();
            assert_eq!(
                cell.fg,
                Color::Rgb(expected_fg[0], expected_fg[1], expected_fg[2])
            );
            assert_eq!(
                cell.bg,
                Color::Rgb(expected_bg[0], expected_bg[1], expected_bg[2])
            );
        }
    }
}
