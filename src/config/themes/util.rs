//! Shared helpers for RGB-based built-in themes, deriving surface tones and muted text from a base
//! `bg` / `ink` pair.  The indexed built-ins hand-pick their tints from the 6×6×6 cube instead.

use ratatui::style::Color;

/// An RGB [`Color`] from a packed `0xRRGGBB` literal, so palette tables read as a hex column.
pub fn rgb(hex: u32) -> Color {
    Color::Rgb(
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    )
}

/// Blend `a` toward `b` by `t`.  RGB only; other variants return `a` unchanged.
pub fn blend(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let mix = |x: u8, y: u8| {
                (x as f32 * (1.0 - t) + y as f32 * t)
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
        }
        _ => a,
    }
}

/// Increase chroma without shifting hue by pushing each channel from the color's own mean.  Pure
/// greys stay grey; non-RGB colors pass through.
pub fn saturate(c: Color, amount: f32) -> Color {
    let Color::Rgb(r, g, b) = c else { return c };
    let avg = (r as f32 + g as f32 + b as f32) / 3.0;
    let push = |x: u8| {
        let d = x as f32 - avg;
        (avg + d * (1.0 + amount)).round().clamp(0.0, 255.0) as u8
    };
    Color::Rgb(push(r), push(g), push(b))
}

/// Relative luminance in `0.0..=1.0`.  `None` for non-RGB colors, whose real value depends on the
/// terminal palette.
pub fn luminance(c: Color) -> Option<f32> {
    let Color::Rgb(r, g, b) = c else { return None };
    Some((0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0)
}

/// Whichever of `a` / `b` contrasts more with `bg` by absolute luminance difference, so a theme
/// whose `accent` sits near its `text` doesn't render selected text as mud.  Falls back to `a` when
/// luminance is unavailable, preserving the caller's default.
pub fn best_contrast(bg: Color, a: Color, b: Color) -> Color {
    match (luminance(bg), luminance(a), luminance(b)) {
        (Some(l_bg), Some(l_a), Some(l_b)) => {
            if (l_a - l_bg).abs() >= (l_b - l_bg).abs() {
                a
            } else {
                b
            }
        }
        _ => a,
    }
}

/// WCAG contrast ratio in `1.0..=21.0`; `None` for non-RGB colors, as with [`luminance`].
pub fn contrast_ratio(a: Color, b: Color) -> Option<f32> {
    // [`luminance`] averages *gamma-encoded* channels, which is what `best_contrast`'s relative
    // comparison wants; a WCAG ratio needs linearized ones, hence the separate conversion.
    fn linear(c: Color) -> Option<f32> {
        let Color::Rgb(r, g, b) = c else { return None };
        let f = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        Some(0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b))
    }
    let (x, y) = (linear(a)?, linear(b)?);
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    Some((hi + 0.05) / (lo + 0.05))
}

/// How far toward `ink` one lift step moves a color; small enough that a color clearing the bar
/// early keeps most of its hue.
const LEGIBILITY_STEP: f32 = 0.1;

/// `fg` if it already reaches `min` contrast against `bg`, else the same hue blended toward `ink`
/// until it does.
///
/// For the `syntax_*` styles, whose foregrounds come from palette slots chosen by *role* rather
/// than measured against the code surface: a mid-tone accent that reads as a heading on the page
/// can be near-invisible on a code block's wash.  `ink` is legible on that surface by construction,
/// so blending toward it raises separation while keeping the hue that tells token classes apart.
///
/// Non-RGB colors return `fg` unchanged — [`blend`] is a no-op there and the loop would not
/// converge — so the indexed built-ins hand-pick their `syntax_*` colors.
pub fn legible_on(bg: Color, fg: Color, ink: Color, min: f32) -> Color {
    if !matches!(
        (bg, fg, ink),
        (Color::Rgb(..), Color::Rgb(..), Color::Rgb(..))
    ) {
        return fg;
    }
    let mut out = fg;
    let mut t = 0.0;
    while contrast_ratio(out, bg).is_some_and(|c| c < min) && t < 1.0 {
        t += LEGIBILITY_STEP;
        out = blend(fg, ink, t);
    }
    out
}

/// Chroma boost for derived chrome surfaces, picked so the tint reads as warm or cool grey rather
/// than a recognizable hue.  Above ~1.0 these start to look like colored panels.
const CHROME_SATURATION_BOOST: f32 = 1.0;

/// A chrome surface lifted from `bg` toward `ink` by `t`, then saturated so it keeps a hint of
/// `bg`'s tint rather than reading as flat grey.  Used for the `surface*` palette slots.
pub fn chrome(bg: Color, ink: Color, t: f32) -> Color {
    saturate(blend(bg, ink, t), CHROME_SATURATION_BOOST)
}
