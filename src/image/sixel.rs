//! Sixel payload for inline-math atoms on **Windows Terminal** — the WT sibling of the kitty
//! placement the spike uses on WezTerm.
//!
//! Why a second emitter at all: WT has no kitty graphics and no iTerm2 inline images; it has
//! **sixel**, and its sixel lands in a place the kitty path cannot match for our purpose — the
//! *text row* owns a pixel buffer (`ImageSlice`), the renderer paints it **on top of** the row's
//! glyphs, and any text written over those cells erases the pixels again.  So on WT the model costs
//! less bookkeeping than kitty, not more: no image ids, no transmit/placement split, and no delete
//! is ever needed — re-drawing the source cells *is* the reveal.
//!
//! Everything below is a rule measured on a real Windows Terminal.  Each number is the measurement
//! itself — read back from a frame capture, pixel by pixel — rather than an estimate from code:
//!
//! * **Declare exactly [`NOMINAL_H`] px (4 bands).**  WT maps a fixed **10×20 px** nominal cell onto
//!   the real one (`SixelParser::CellSizeForLevel`, and no conformance level is passed when the
//!   parser is built).  A shorter image is *stretched* to fill the cell (18 px was measured to
//!   occupy the full row, an extra ×1.11 of vertical distortion), and a taller one spills: a 24 px
//!   image put 7 real px of ink into the row below.  So the ink is authored in the top
//!   [`NOMINAL_H`] rows and the 4th band's remaining rows stay unpainted — WT's flush copies
//!   non-transparent pixels only.
//!   That fixed grid is also the **resolution ceiling**: every cell gets at most 10×20 source pixels,
//!   upscaled to the real cell (14×32 here, i.e. ~×1.4 and ~×1.6), so the image is softer than a
//!   kitty placement at the same size.  No payload trick fixes that; sharpening the raster on the
//!   way *down* to the grid is the lever that is left.
//! * **Put the baseline at [`BASELINE_Y`]** = 0.75 × 20, the same ratio the kitty path uses.  The
//!   nominal-to-real mapping is linear from the cell top, and the measured result is the formula's
//!   baseline line landing **pixel-exactly on the text's baseline** (four rows independently:
//!   `label ink bottom 225 == red line y 225-226`).
//! * **Paint the whole box, page colour included** — two palette entries, no transparency.  A
//!   transparent sixel shows whatever the cells hold underneath, and the atom's cells are styled
//!   `theme.code_span` (the same chip the literal source sits on), while the erase sweep the symbol
//!   begins with is an `ECH` that fills with the *current* SGR background — i.e. the chip.  One
//!   transparent formula therefore renders on a visible chip; the kitty path never showed this
//!   because its bitmap is flattened onto the page and therefore opaque.  Painting the background
//!   explicitly is the fix, and it stays out of ratatui's style-diff model, which injecting our own
//!   SGR background would corrupt.
//! * One payload **per atom, per frame**.  Unlike kitty there is nothing to reuse between frames,
//!   but the measured cost is ~10 µs per atom (~0.12 ms for twelve), against a 16.7 ms frame
//!   budget: re-sending is affordable and simpler than caching.
//!
//! ## Why not `icy_sixel`, which `ratatui-image` already links?
//!
//! It *is* in the binary (`ratatui-image` depends on it unconditionally and does not re-export it),
//! and it does hand back its bytes (`icy_sixel::sixel_encode(rgba, w, h, &EncodeOptions) ->
//! Result<String>`, `icy_sixel-0.5.0/src/encoder.rs:81-89`), so "reuse the encoder that is already
//! here" is tempting.  It is still the wrong encoder for this payload, for three measured reasons:
//!
//! * it emits **no DECGRA raster-attribute block** (`encoder.rs:200-206` writes only
//!   `ESC P<p1>;<p2>;0q`), and that block is what fixes the target terminal's six-pixel band height;
//!   the whole 10x20 grid and [`BASELINE_Y`] are derived from that number;
//! * it cannot **declare an extent independent of what it paints** — bands are
//!   `height.div_ceil(6)` (`encoder.rs:225`) — while this payload must *declare* 20 px and leave the
//!   4th band's tail unpainted, or the terminal slices that tail into the next text row (measured:
//!   7 real px of ink in the row below);
//! * its palette comes from **quantising the input pixels** (`encoder.rs:126-140`), so the page
//!   colour behind a formula would be an approximation of the theme's background instead of the
//!   same colour — a faint rectangle around every formula, i.e. exactly the artefact the page-colour
//!   pass exists to remove.
//!
//! `ratatui-image`'s own wrapper is not a general encoder either: it feeds the *image's* own
//! width/height through (`ratatui-image-11.0.6/src/protocol/sixel.rs:39-48`) and prepends its own
//! clear-area escapes (`:58-64`) before exposing `data: String` for a full-area draw, whereas this
//! payload is spliced into a single cell's symbol.
//!
//! The payload is returned as a `String` because every byte of it is ASCII text: the caller splices
//! it into a cell's symbol, next to the escape conventions
//! `image::inline_math::placement_symbol` already establishes — an erase sweep before it (clearing
//! the source glyphs and the chip), and a cursor position after it (WT's own sixel bookkeeping moves
//! the text cursor to the image's last row, so the trailing position is what puts ratatui's cell
//! cursor back where it believes it is).

use image::{imageops, DynamicImage, GenericImageView};

/// WT's nominal cell for sixel, in pixels — fixed, not the real font's cell.
const NOMINAL_W: u32 = 10;
const NOMINAL_H: u32 = 20;

/// The text baseline inside the nominal cell, in pixels: 0.75 × 20, measured to land on the text's
/// own baseline.
pub const BASELINE_Y: u32 = 15;

/// Sixel bands are 6 px tall; 20 px of content needs four of them.
const BAND_H: u32 = 6;

/// Palette entries, in the order they are declared.
const INK: u8 = 0;
const PAGE: u8 = 1;

/// `true` for a pixel the ink, not the page.
fn is_ink(pixel: [u8; 3], ink: [u8; 3], background: [u8; 3]) -> bool {
    // Nearest of the two palette entries, so a soft edge lands on the side it is closer to instead
    // of always counting as ink and thickening the glyph.
    max_channel_diff(pixel, ink) < max_channel_diff(pixel, background)
}

/// Encode `bitmap` as one sixel image `cells` columns wide, authored on WT's nominal grid.
pub fn encode(bitmap: &DynamicImage, cells: u16, ink: [u8; 3], background: [u8; 3]) -> String {
    let w = (cells.max(1) as u32) * NOMINAL_W;
    let grid = classify(bitmap, w, ink, background);

    let mut out = String::with_capacity(w as usize * 12 + 96);
    // P1 = 0 (no macro), P2 = 1 (the area *outside* the image stays untouched), P3 = 0.
    out.push_str("\x1bP0;1;0q");
    // Raster attributes: 1:1 pixel aspect (this is what fixes WT's `_sixelHeight` at 6 px), then the
    // declared extent.  Declaring 20 px while the 4th band writes nothing is what keeps the image
    // inside its cell — see the module docs.
    out.push_str(&format!("\"1;1;{w};{NOMINAL_H}"));
    for (entry, colour) in [(INK, ink), (PAGE, background)] {
        out.push_str(&format!(
            "#{entry};2;{};{};{}",
            pct(colour[0]),
            pct(colour[1]),
            pct(colour[2])
        ));
    }

    for band in 0..NOMINAL_H.div_ceil(BAND_H) {
        if band > 0 {
            out.push('-');
        }
        for (pass, colour) in [INK, PAGE].into_iter().enumerate() {
            if pass > 0 {
                // Graphics carriage return: the same band, from its first column.
                out.push('$');
            }
            let masks: Vec<u8> = (0..w)
                .map(|x| {
                    let mut m = 0u8;
                    for i in 0..BAND_H {
                        let y = band * BAND_H + i;
                        // Rows at or below the declared height exist only as the 4th band's tail,
                        // and must stay unpainted — see the module docs.
                        if y < NOMINAL_H && grid[y as usize][x as usize] == colour {
                            m |= 1 << i;
                        }
                    }
                    m
                })
                .collect();
            out.push_str(&format!("#{colour}"));
            push_rle(&mut out, &masks);
        }
    }
    out.push_str("\x1b\\");
    out
}

/// Every pixel of the box, on a `w × NOMINAL_H` grid resampled from `bitmap`: [`INK`] or [`PAGE`].
///
/// Nothing is left transparent on purpose — a transparent pixel would show the `code_span` chip the
/// atom's cells carry (see the module docs).
fn classify(bitmap: &DynamicImage, w: u32, ink: [u8; 3], background: [u8; 3]) -> Vec<Vec<u8>> {
    let (src_w, src_h) = bitmap.dimensions();
    let rgba = bitmap.to_rgba8();
    let resized = if (src_w, src_h) == (w, NOMINAL_H) {
        rgba
    } else {
        imageops::resize(&rgba, w, NOMINAL_H, imageops::FilterType::Triangle)
    };
    (0..NOMINAL_H)
        .map(|y| {
            (0..w)
                .map(|x| {
                    let p = resized.get_pixel(x, y).0;
                    if p[3] < 128 {
                        // Nothing was drawn there; the page is what shows.
                        PAGE
                    } else if is_ink([p[0], p[1], p[2]], ink, background) {
                        INK
                    } else {
                        PAGE
                    }
                })
                .collect()
        })
        .collect()
}

fn max_channel_diff(a: [u8; 3], b: [u8; 3]) -> u8 {
    (0..3).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0)
}

/// Sixel's palette is in percent, 0-100.
fn pct(v: u8) -> u32 {
    (u32::from(v) * 100 + 127) / 255
}

/// Sixel data: `chr(63 + mask)` per column, with `!<count>` run-length encoding for repeats.
fn push_rle(out: &mut String, masks: &[u8]) {
    let mut i = 0;
    while i < masks.len() {
        let mut j = i;
        while j < masks.len() && masks[j] == masks[i] {
            j += 1;
        }
        let n = j - i;
        let ch = char::from(63 + masks[i]);
        if n > 3 {
            out.push_str(&format!("!{n}"));
            out.push(ch);
        } else {
            for _ in 0..n {
                out.push(ch);
            }
        }
        i = j;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    const PAGE_RGB: [u8; 3] = [0x1a, 0x1a, 0x1a];
    const INK_RGB: [u8; 3] = [0xcc, 0xcc, 0xcc];

    /// A bitmap of the shape the spike rasterizes: `cells` real cells wide, one cell tall, with a
    /// solid bar at 0.75 of the cell height — the baseline the encoder has to land on `BASELINE_Y`.
    fn bitmap_with_baseline_bar(cells: u16, cell: (u32, u32)) -> DynamicImage {
        let mut img = image::RgbaImage::from_pixel(
            cells as u32 * cell.0,
            cell.1,
            Rgba([PAGE_RGB[0], PAGE_RGB[1], PAGE_RGB[2], 255]),
        );
        let y = cell.1 * 3 / 4;
        for x in 0..img.width() {
            img.put_pixel(x, y, Rgba([INK_RGB[0], INK_RGB[1], INK_RGB[2], 255]));
        }
        DynamicImage::ImageRgba8(img)
    }

    /// What a terminal would see: for each pixel, the palette entry that painted it, or `None`.
    fn decode(payload: &str) -> Vec<Vec<Option<u8>>> {
        let text = payload;
        let header = &text[text.find("q\"").expect("header") + 2..];
        let mut it = header[..header
            .find(|c: char| !(c.is_ascii_digit() || c == ';' || c == '"'))
            .unwrap()]
            .split(';');
        // `"Pan;Pad;Ph;Pv` - the aspect pair comes first.
        let _pan = it.next().unwrap();
        let _pad = it.next().unwrap();
        let w: usize = it.next().unwrap().parse().unwrap();
        let h: usize = it.next().unwrap().parse().unwrap();
        assert_eq!(
            h, NOMINAL_H as usize,
            "always declares the full nominal height"
        );

        // Bands span 24 rows even though only 20 are declared; rows 20..23 are the 4th band's tail
        // and the tests assert they stay unpainted (WT slices a painted tail into the row below).
        let mut rows = vec![vec![None; w]; h.div_ceil(6) * 6];
        let mut body = &text[text.find("q\"").expect("header") + 2..];
        body = &body[body.find('#').expect("palette")..];
        let mut colour = 0u8;
        let mut band = 0usize;
        let mut x = 0usize;
        let bytes = body.as_bytes();
        let mut k = 0usize;
        while k < bytes.len() {
            match bytes[k] {
                b'#' => {
                    // `#<entry>;2;r;g;b` - consume the spec, then keep painting with that entry.
                    let spec: String = body[k + 1..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || *c == ';')
                        .collect();
                    colour = spec.split(';').next().unwrap().parse().unwrap();
                    k += 1 + spec.len();
                }
                b'-' => {
                    band += 1;
                    x = 0;
                    k += 1;
                }
                b'$' => {
                    x = 0;
                    k += 1;
                }
                b'!' => {
                    let digits: String = body[k + 1..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    k += 1 + digits.len();
                    let ch = bytes[k];
                    let count: usize = digits.parse().unwrap();
                    paint(&mut rows, band, &mut x, w, ch, colour, count);
                    k += 1;
                }
                0x1b => break,
                _ => {
                    paint(&mut rows, band, &mut x, w, bytes[k], colour, 1);
                    k += 1;
                }
            }
        }
        rows
    }

    fn paint(
        rows: &mut [Vec<Option<u8>>],
        band: usize,
        x: &mut usize,
        w: usize,
        ch: u8,
        colour: u8,
        count: usize,
    ) {
        let mask = ch - 63;
        for _ in 0..count {
            if *x < w {
                for i in 0..6usize {
                    if mask & (1 << i) != 0 {
                        let y = band * 6 + i;
                        if y < rows.len() {
                            rows[y][*x] = Some(colour);
                        }
                    }
                }
            }
            *x += 1;
        }
    }

    fn ink_rows(payload: &str) -> Vec<usize> {
        decode(payload)
            .iter()
            .enumerate()
            .filter(|(_, row)| row.contains(&Some(INK)))
            .map(|(y, _)| y)
            .collect()
    }

    #[test]
    fn a_payload_declares_twenty_rows_two_colours_and_three_band_breaks() {
        let text = encode(&bitmap_with_baseline_bar(2, (14, 32)), 2, INK_RGB, PAGE_RGB);
        assert!(text.starts_with("\x1bP0;1;0q"), "DCS");
        assert!(
            text.contains("\"1;1;20;20"),
            "raster attributes: 1:1 aspect, 20x20 px"
        );
        assert!(text.contains("#0;2;"), "the ink entry");
        assert!(text.contains("#1;2;"), "the page entry");
        assert!(text.ends_with("\x1b\\"), "string terminator");
        assert_eq!(text.matches('-').count(), 3, "four bands, three separators");
        assert_eq!(text.matches('$').count(), 4, "one colour switch per band");
    }

    #[test]
    fn the_ink_lands_on_the_baseline_row() {
        let text = encode(&bitmap_with_baseline_bar(2, (14, 32)), 2, INK_RGB, PAGE_RGB);
        assert_eq!(
            ink_rows(&text),
            vec![BASELINE_Y as usize],
            "the raster's 0.75 bar must occupy row {BASELINE_Y} and nothing else"
        );
    }

    /// The fix for the chip: not one pixel of the box is left unpainted, so nothing of the cell
    /// (its `code_span` background included) can show through the formula.
    #[test]
    fn every_pixel_of_the_box_is_painted_with_ink_or_page() {
        let rows = decode(&encode(
            &bitmap_with_baseline_bar(2, (14, 32)),
            2,
            INK_RGB,
            PAGE_RGB,
        ));
        for (y, row) in rows.iter().enumerate().take(NOMINAL_H as usize) {
            assert!(
                row.iter().all(Option::is_some),
                "row {y} has an unpainted pixel: {row:?}"
            );
        }
        assert!(
            rows[..BASELINE_Y as usize]
                .iter()
                .all(|r| r.contains(&Some(PAGE))),
            "the rows above the baseline are page, not ink"
        );
    }

    #[test]
    fn nothing_below_the_declared_height_is_painted() {
        // The 4th band's rows 20..23 must stay unpainted: WT slices a painted tail into the row
        // below (measured: 7 real px of spill out of a 24 px image).
        let rows = decode(&encode(
            &bitmap_with_baseline_bar(1, (14, 32)),
            1,
            INK_RGB,
            PAGE_RGB,
        ));
        assert_eq!(rows.len(), 24, "four bands of data exist");
        assert!(
            rows[20..].iter().all(|r| r.iter().all(Option::is_none)),
            "rows 20..23 belong to the next text row - they must stay unpainted"
        );
    }

    #[test]
    fn an_ink_free_bitmap_encodes_as_a_page_only_box() {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            28,
            32,
            Rgba([PAGE_RGB[0], PAGE_RGB[1], PAGE_RGB[2], 255]),
        ));
        let rows = decode(&encode(&img, 2, INK_RGB, PAGE_RGB));
        assert!(
            rows.iter().all(|r| r.iter().all(|c| *c != Some(INK))),
            "no ink anywhere"
        );
        assert!(
            rows[..NOMINAL_H as usize]
                .iter()
                .all(|r| r.iter().all(|c| *c == Some(PAGE))),
            "and the whole box is page"
        );
    }

    #[test]
    fn a_pixel_is_page_when_nothing_was_drawn_there() {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            28,
            32,
            Rgba([0, 0, 0, 0]), // fully transparent: never painted by the rasterizer
        ));
        let rows = decode(&encode(&img, 2, INK_RGB, PAGE_RGB));
        assert!(
            rows[..NOMINAL_H as usize]
                .iter()
                .all(|r| r.iter().all(|c| *c == Some(PAGE))),
            "transparent source pixels become the page, not a hole"
        );
    }
}
