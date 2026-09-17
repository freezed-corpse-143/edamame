//! LaTeX display math → SVG → `DynamicImage` pipeline.
//!
//! [`resolve_latex`] is the public entry point (used by the App decode
//! worker, same contract as [`super::mermaid::resolve_mermaid`]).  The
//! pipeline is pure Rust — no node, no system TeX: RaTeX parses the LaTeX,
//! lays it out in display style, flattens to a display list, and serializes
//! an SVG.  We emit **`<text>`** (not embedded glyph outlines): RaTeX names
//! its faces `font-family="KaTeX_*"`, and the shared fontdb — which carries
//! the bundled KaTeX TTFs, see [`crate::image::svg`] — resolves them during
//! the rasterize step, exactly how the mermaid path renders.  That keeps a
//! single font subsystem rather than the parallel `ab_glyph` stack the
//! `standalone`/`embed-fonts` features would pull in.
//!
//! RaTeX 0.1.x is pre-1.0 with known panic bugs, so the render is wrapped in
//! `catch_unwind` like the mermaid renderer.  The shared cache-key URL
//! scheme, [`DiagramSource`](super::common::DiagramSource) and
//! [`DiagramError`] live in [`super::common`].

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::image::{rasterize_svg, LoadedImage, SvgScaleMode, SvgSizing};

use super::common::{panic_message, DiagramError};

/// Maximum LaTeX source length we will attempt to render.  RaTeX has no
/// internal bound, so an over-cap formula gets the same clean failure →
/// placeholder path as an over-cap mermaid diagram.
const MAX_LATEX_SOURCE_BYTES: usize = 64 * 1024;

/// What one SVG point is in pixels (96 dpi / 72 pt).  Named once, because both the em-per-cell
/// factor and the raster sizing are derived from it and must not drift apart.
const PT_TO_PX: f64 = 96.0 / 72.0;

/// Cell-height → RaTeX `font_size` conversion factor.
///
/// Two unit mismatches stand between the terminal's cell *height* (pixels)
/// and the value RaTeX expects (user units per em):
///
/// * RaTeX emits its SVG labelled in `pt`, and usvg rasterizes CSS `pt` at
///   96 dpi — one `pt` becomes 96/72 px.
/// * A terminal cell spans more than one text em: ascent + descent plus
///   the terminal's configured line height ≈ 1.2–1.4 em in practice.
///
/// The `1 / (96/72 × 1.25)` term lands the formula's x-height on one text
/// em (1.25 is the middle of the common 1.2–1.4 line-height range); the
/// leading `1.25` then sets display math ~25% larger than the surrounding
/// body text, the way typeset display equations are conventionally set —
/// prominent, and large enough to keep subscripts and superscripts legible
/// in the terminal.  Exactness is impossible without font metrics from the
/// terminal, so the factor targets a look within ±10% across terminals.
const LATEX_EM_TO_CELL: f64 = 1.25 / (PT_TO_PX * 1.25);

/// Internal margin baked into every formula's SVG, as a fraction of its em
/// (RaTeX's `padding` is in the same user units as the glyph coordinates,
/// so an em-relative value scales with the formula).  Keeps glyphs off the
/// image edge so a formula never rubs against the frame — visible in the
/// HTML export, where the figure sits on white, and honoured in-app once
/// the margin flattens onto the document background.  Because it lives in
/// the shared [`render_latex_svg`] output, the in-app raster and the export
/// PNG get the identical margin.
const LATEX_PADDING_EMS: f64 = 0.3;

/// Render a LaTeX display-math source all the way to a `LoadedImage`,
/// suitable for dropping straight into the image cache — same contract as
/// [`super::mermaid::resolve_mermaid`].
///
/// Sizing: the SVG's user-unit scale is RaTeX's `font_size` (em units).
/// We derive it from the terminal's cell pixel height via [`LATEX_EM_TO_CELL`]
/// so the formula's x-height matches body text (not one full cell), drop
/// RaTeX's default padding (a fixed frame dwarfing the glyphs), then
/// rasterize with `SvgScaleMode::Natural` (downscale only) so a formula
/// wider than the column shrinks to fit but never balloons — unlike
/// Mermaid, a formula has a meaningful natural size.  The rasterized image
/// is then fitted to the cell grid ([`fit_latex_to_cell_grid`]): the glyphs
/// are painted in `fg` (the theme's text colour) and flattened onto `bg`
/// (the document background) so dark themes stay legible and scrolling
/// doesn't smear a transparent formula black.
pub fn resolve_latex(
    url: String,
    source: &str,
    max_cells: Option<(u16, u16)>,
    font_size: Option<(u16, u16)>,
    fg: [u8; 4],
    bg: [u8; 4],
) -> Result<LoadedImage, DiagramError> {
    let svg = render_latex_svg(source, fg, font_size)?;
    let image = rasterize_svg(
        &svg,
        SvgSizing {
            envelope: max_cells,
            font_size,
            mode: SvgScaleMode::Natural,
        },
        None, // transparent — fitted / flattened onto `bg` below
    )
    .map_err(DiagramError::from)?;
    Ok(LoadedImage {
        url,
        image: fit_latex_to_cell_grid(image, font_size, bg),
        scratch: None,
    })
}

/// `ratex_svg` writes the SVG's size in **points** (`width="…pt"`) while every coordinate in it is
/// a user unit, and `usvg` resolves points at 96 dpi — so a formula laid out `h` user units tall
/// rasterizes `h × 4/3` pixels tall.  Every pixel budget in [`render_inline_latex`] folds this in;
/// without it the baseline lands a third of a formula too low (measured: 3 px on a 20 px cell,
/// which is what the ink-bottom test in this module pins).
const SVG_PT_TO_PX: f64 = PT_TO_PX;

/// Inline padding, as a fraction of the em.  A hair of margin, not display math's
/// `LATEX_PADDING_EMS` (0.3em): 0.3em is a quarter of the cell budget here, and it is the
/// *descent* side that runs out first.
const INLINE_PAD_EMS: f64 = 0.05;

/// The em an inline formula is set at, as a fraction of the cell height.
///
/// Display math's [`LATEX_EM_TO_CELL`] is the starting point — the same cell-height → em factor
/// `resolve_latex` uses — and inline math takes 0.74 of it, measured rather than derived: at the
/// display factor a formula's capital `Y` rasterizes 21 px against the text's own 18 px capitals
/// and ascenders in the same *frame* — a 21 px capital against the text's own 18 px — and
/// 18/21 ≈ 0.86 of that em, i.e. 0.74 of the display factor.  A formula sharing
/// a line with prose has to read as part of the sentence; display math keeps the larger factor on
/// purpose, being a display element.
///
/// Font-dependent, like the baseline ratio: `EDAMAME_INLINE_MATH_SIZE` scales it for re-measuring
/// against another terminal font.
const INLINE_EM_TO_CELL: f64 = LATEX_EM_TO_CELL * 0.74;

/// `EDAMAME_INLINE_MATH_SIZE`, defaulting to 1: a multiplier on [`INLINE_EM_TO_CELL`], so the
/// inline formula's size can be re-measured per font without a rebuild.
fn inline_size_factor() -> f64 {
    static FACTOR: std::sync::LazyLock<f64> = std::sync::LazyLock::new(|| {
        std::env::var("EDAMAME_INLINE_MATH_SIZE")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(1.0)
            .clamp(0.4, 1.5)
    });
    *FACTOR
}

/// One inline formula's layout plus the em it will be rasterized at.
///
/// The width measurement and the raster both go through this, which is the point: the atom's cell
/// width has to be the ink's width *at the size the image will actually have*, or the text after
/// the formula lands in the wrong place.
struct InlinePlan {
    list: ratex_types::display_item::DisplayList,
    em: f64,
}

impl InlinePlan {
    fn layout(
        source: &str,
        font_size: Option<(u16, u16)>,
        fg: [u8; 4],
        ratio: f32,
    ) -> Result<Self, DiagramError> {
        use ratex_layout::layout_options::LayoutOptions;
        use ratex_layout::{layout, to_display_list};
        use ratex_parser::parse;
        use ratex_types::color::Color;
        use ratex_types::math_style::MathStyle;

        if source.len() > MAX_LATEX_SOURCE_BYTES {
            return Err(DiagramError::RenderFailed(format!(
                "latex source too large: {} bytes (max {MAX_LATEX_SOURCE_BYTES})",
                source.len()
            )));
        }
        let ast = parse(source).map_err(|e| DiagramError::RenderFailed(format!("{e:#}")))?;
        let [r, g, b, a] = fg;
        let options = LayoutOptions::default()
            .with_style(MathStyle::Text)
            .with_color(Color::new(
                f32::from(r) / 255.0,
                f32::from(g) / 255.0,
                f32::from(b) / 255.0,
                f32::from(a) / 255.0,
            ));
        let list = to_display_list(&layout(&ast, &options));

        let (_cell_w, cell_h) = font_size.unwrap_or((8, 16));
        let cell_h = f64::from(cell_h.max(1));
        let ratio = f64::from(ratio.clamp(0.40, 0.95));
        let ascent = list.height.max(0.01);
        let depth = list.depth.max(0.0);
        // The whole budget is in *raster pixels*; the em that produces them is a user unit, so
        // every constraint is divided by the pt→px factor before it becomes an em.
        //
        // Capped at the em display math uses for body-sized text: the two room constraints below
        // only say the ink has to fit above and below the baseline, and a *short* formula (a lone
        // `x`) satisfies them at a huge em — which rendered inline letters noticeably larger than
        // the prose around them.  `LATEX_EM_TO_CELL` is the same cell-height → em factor
        // `resolve_latex` uses to match the body text's x-height.
        let body_em = cell_h * INLINE_EM_TO_CELL * inline_size_factor();
        let em = ((ratio * cell_h / (ascent + INLINE_PAD_EMS))
            .min((1.0 - ratio) * cell_h / (depth + INLINE_PAD_EMS))
            / SVG_PT_TO_PX)
            .min(body_em);
        Ok(Self { list, em })
    }

    fn pad(&self) -> f64 {
        self.em * INLINE_PAD_EMS
    }

    fn ascent(&self) -> f64 {
        self.list.height.max(0.01)
    }

    /// The whole SVG's size in raster pixels — ink plus padding, at the pt→px factor the SVG's
    /// `pt` units imply.
    fn px(&self) -> (f64, f64) {
        let pad = self.pad();
        (
            (self.list.width * self.em + 2.0 * pad) * SVG_PT_TO_PX,
            ((self.list.height + self.list.depth) * self.em + 2.0 * pad) * SVG_PT_TO_PX,
        )
    }
}

/// Cells an inline formula needs on its own row at `font_size`: its ink width rounded up, measured
/// at the same em the raster will use.
///
/// This is what replaces "as many cells as the source text is wide" — the source's own width is
/// the wrong number (the delimiters and the ASCII transcription of `\alpha` have nothing to do
/// with the ink), and reserving it leaves visible whitespace after every short formula.
///
/// `None` when RaTeX cannot lay the formula out, or the width is not representable in cells; the
/// caller then keeps the literal source, which is the same fallback a failed diagram gets.
pub fn inline_latex_width_cells(
    source: &str,
    font_size: Option<(u16, u16)>,
    ratio: f32,
) -> Option<u16> {
    let (cell_w, _) = font_size.unwrap_or((8, 16));
    let cell_w = f64::from(cell_w.max(1));
    // The colour cannot change the metrics; white keeps this call independent of the theme.
    let plan = InlinePlan::layout(source, font_size, [255, 255, 255, 255], ratio).ok()?;
    let (px_w, _) = plan.px();
    u16::try_from((px_w / cell_w).ceil().max(1.0) as i64).ok()
}

/// Render an **inline** formula into a bitmap exactly one cell row tall, with its baseline at
/// `baseline_ratio` of the cell height.
///
/// Display math ([`resolve_latex`]) solves the easy version of this: it owns whole rows, so it
/// lays out in display style and centres itself in an exact number of cells.  An inline formula
/// must share a text row, which changes three things:
///
/// * layout runs in `MathStyle::Text`, not the default display style — otherwise a `\frac`'s
///   parts are set at display size and nothing fits one row;
/// * the em comes from the *cell* budget, not from [`LATEX_EM_TO_CELL`]: the ink has to fit above
///   and below the intended baseline, so the em is the smaller of the two room constraints—
///   `ascent·em ≤ ratio·cell_h`, `depth·em ≤ (1-ratio)·cell_h`;
/// * the returned image is the whole `cells_w × 1` cell box with the formula pasted at the pixel
///   offset that lands its baseline on `baseline_ratio·cell_h`.  Filling the box (rather than
///   returning the ink's own extent) is what keeps the terminal from stretching a narrow formula
///   across the atom's cells, and what leaves the text on either side exactly where the source
///   put it.
///
/// `cells_w` is the atom's reserved width, which the caller takes from
/// [`inline_latex_width_cells`] — the *ink's* width, never the source text's.  Reserving the
/// source's width was the first cut, and it left visible whitespace after every short formula
/// (`$Y_1$` is five source characters and about three cells of ink).
pub fn render_inline_latex(
    source: &str,
    cells_w: u16,
    font_size: Option<(u16, u16)>,
    fg: [u8; 4],
    bg: [u8; 4],
    baseline_ratio: f32,
) -> Result<image::DynamicImage, DiagramError> {
    if source.len() > MAX_LATEX_SOURCE_BYTES {
        return Err(DiagramError::RenderFailed(format!(
            "latex source too large: {} bytes (max {MAX_LATEX_SOURCE_BYTES})",
            source.len()
        )));
    }
    let outcome = {
        let _expected = crate::terminal::ExpectedPanic::new();
        catch_unwind(AssertUnwindSafe(|| {
            render_inline_latex_inner(source, cells_w, font_size, fg, bg, baseline_ratio)
        }))
    }
    .map_err(|payload| {
        DiagramError::RenderFailed(format!("latex render panic: {}", panic_message(&payload)))
    })?;
    outcome
}

fn render_inline_latex_inner(
    source: &str,
    cells_w: u16,
    font_size: Option<(u16, u16)>,
    fg: [u8; 4],
    bg: [u8; 4],
    baseline_ratio: f32,
) -> Result<image::DynamicImage, DiagramError> {
    use ratex_svg::{render_to_svg, SvgOptions};

    let plan = InlinePlan::layout(source, font_size, fg, baseline_ratio)?;
    let (cell_w, cell_h) = font_size.unwrap_or((8, 16));
    let (cell_w, cell_h) = (f64::from(cell_w.max(1)), f64::from(cell_h.max(1)));
    let ratio = f64::from(baseline_ratio.clamp(0.40, 0.95));

    let svg = render_to_svg(
        &plan.list,
        &SvgOptions {
            embed_glyphs: false,
            font_size: plan.em,
            padding: plan.pad(),
            ..SvgOptions::default()
        },
    );

    let (raster, scale) = crate::image::rasterize_svg_scaled(
        &svg,
        crate::image::svg::SvgSizing {
            envelope: Some((cells_w.max(1), 1)),
            font_size: Some((cell_w as u16, cell_h as u16)),
            mode: crate::image::svg::SvgScaleMode::Natural,
        },
        None,
    )
    .map_err(DiagramError::from)?;

    // The SVG puts the baseline `pad + ascent·em` from its top *in user units*; the raster scaled
    // that by the pt→px factor and by `scale` (the envelope can force one down when the formula is
    // wider than the atom).
    let baseline_px = (plan.pad() + plan.ascent() * plan.em) * SVG_PT_TO_PX * f64::from(scale);
    let top = (ratio * cell_h - baseline_px).round().max(0.0) as i64;

    let mut canvas = image::ImageBuffer::from_pixel(
        (cells_w.max(1) as u32) * (cell_w as u32),
        cell_h as u32,
        image::Rgba([0, 0, 0, 0]),
    );
    // The box is a whole number of cells and the ink is not, so `ceil` leaves up to a cell of
    // slack; splitting it evenly reads as optical spacing, where leaving it all on the right reads
    // as a missing glyph (and pushes a following comma away from the formula it belongs to).
    let left = ((canvas.width() as i64 - raster.width() as i64) / 2).max(0);
    image::imageops::overlay(&mut canvas, &raster.to_rgba8(), left, top);
    // Opaque, on the document background — *not* left transparent, even though the terminal
    // composites with alpha.  The atom's cells hold the source text, styled `code_span`, whose
    // background the placement's erase sweep (ECH) refills before the image lands: a transparent
    // bitmap would let that chip show straight through the formula's empty pixels.  Painting the
    // page colour instead makes the formula float on the document, and does it only where an
    // image was actually placed — the literal `$x$` elsewhere keeps whatever style it had.
    // Same convention (and the same helper) as display math's `fit_latex_to_cell_grid`.
    Ok(flatten_to_background(
        image::DynamicImage::ImageRgba8(canvas),
        bg,
    ))
}

/// Prepare a formula image for the terminal's cell grid: symmetric
/// breathing room, opaque flatten onto the document background, then
/// vertical centring to an exact whole number of cells.
///
/// The editor reserves image rows in whole cells (`aspect_rows_of` =
/// `ceil(pixels / cell_height)`), and `paint_images` fits the image into
/// the reserved rect **downward-only, flush to the top**.  Without the
/// centring step, the rounding slack between the image's pixel height
/// and the reserved whole-cell height would land entirely below the
/// image as a letter-box gap — the "only blank below the formula" look.
/// Padding up to `rows × cell_height` with the background colour and
/// centring the content in it turns that slack into symmetric top/bottom
/// margins instead, so a formula's vertical rhythm reads like a text
/// line's.
fn fit_latex_to_cell_grid(
    image: image::DynamicImage,
    font_size: Option<(u16, u16)>,
    bg: [u8; 4],
) -> image::DynamicImage {
    let image = add_formula_breathing_room(image, font_size);
    let image = flatten_to_background(image, bg);
    center_on_cell_grid(image, font_size, bg)
}

/// Pad `image` (already opaque, background-coloured) vertically so its
/// height is an exact multiple of the terminal cell height, content
/// centred: `extra = rows × cell_h - height` split equally above and
/// below.
fn center_on_cell_grid(
    image: image::DynamicImage,
    font_size: Option<(u16, u16)>,
    bg: [u8; 4],
) -> image::DynamicImage {
    use image::{GenericImageView, ImageBuffer, Rgba};
    let cell_h = u32::from(font_size.map_or(16, |(_, h)| h.max(1)));
    let (w, h) = image.dimensions();
    let rows = h.div_ceil(cell_h).max(1);
    let total = rows * cell_h;
    if total <= h {
        return image;
    }
    let extra = total - h;
    let top = extra / 2;
    let mut canvas = ImageBuffer::from_pixel(w, total, Rgba([bg[0], bg[1], bg[2], 255]));
    image::imageops::overlay(&mut canvas, &image, 0, i64::from(top));
    image::DynamicImage::ImageRgba8(canvas)
}

/// Composite a formula image onto the document background colour,
/// replacing transparency with opaque `bg`.
///
/// Two consumers need an opaque image:
///
/// * **Halfblocks (active scroll / partial visibility)** encode each cell
///   through `to_rgb8()`, which *drops the alpha channel* — a transparent
///   pixel's RGB is read as-is, and formula transparency is
///   `Rgba([0,0,0,0])`, i.e. black.  With the native protocol idling
///   during scroll (`paint_images` falls back to the halfblocks scratch
///   while `is_scrolling`), every transparent region around a formula
///   painted black — the "black blob while scrolling" bug.  Flattened
///   onto the document `bg`, those regions encode as `bg`, which is
///   exactly what the native composite shows.
/// * **The letter-box / trailing margin** a formula's rect reserves
///   (the rect spans the whole column): the same reasoning — encode as
///   `bg`, not black.
///
/// Result is visually identical to the transparent composite whenever the
/// terminal honours alpha (native kitty/iTerm2/sixel paths), because the
/// cells beneath the image are painted with the same document `bg`.
fn flatten_to_background(image: image::DynamicImage, bg: [u8; 4]) -> image::DynamicImage {
    use image::GenericImageView;
    let [br, bg_, bb, ba] = bg;
    let (w, h) = image.dimensions();
    let mut out = image::ImageBuffer::new(w, h);
    let src = image.to_rgba8();
    for (x, y, px) in src.enumerate_pixels() {
        let [r, g, b, a] = px.0;
        // Straight alpha-over: the formula PNG is transparent or fully
        // opaque in practice (no partial coverage), but stay exact for
        // antialiased glyph edges.
        let alpha = f32::from(a) / 255.0;
        let ba_ = f32::from(ba) / 255.0;
        let mix = |s: u8, d: u8| {
            (f32::from(s) * alpha + f32::from(d) * ba_ * (1.0 - alpha)).round() as u8
        };
        out.put_pixel(
            x,
            y,
            image::Rgba([mix(r, br), mix(g, bg_), mix(b, bb), 255]),
        );
    }
    image::DynamicImage::ImageRgba8(out)
}

/// Pad a formula image with transparent rows above and below so its
/// vertical rhythm matches the text grid.
///
/// Text lines carry their own inter-line gap: a terminal cell is taller
/// than the glyph box (WezTerm line-height 1.15, most fonts 1.2+), so two
/// text lines leave roughly half a cell's worth of background between
/// glyph boxes on each side.  A rendered formula has no such built-in
/// margin — `paint_images` overlays it flush against the reserved cell
/// rect's top edge — so formulas and images sit visually tighter against
/// their neighbours than text does.  ~1/10 of the cell height per side
/// restores the look of an ordinary line gap.  The extra rows are
/// transparent, so layout, aspect-row accounting (`aspect_rows_of`) and
/// the block's reserved height all follow automatically from the new
/// dimensions.
fn add_formula_breathing_room(
    image: image::DynamicImage,
    font_size: Option<(u16, u16)>,
) -> image::DynamicImage {
    use image::imageops::overlay;
    use image::{GenericImageView, ImageBuffer, Rgba};
    let cell_h = u32::from(font_size.map_or(16, |(_, h)| h.max(1)));
    let pad = (cell_h / 10).clamp(1, 6);
    let (w, h) = image.dimensions();
    let mut canvas = ImageBuffer::from_pixel(w, h + 2 * pad, Rgba([0, 0, 0, 0]));
    overlay(&mut canvas, &image.to_rgba8(), 0, i64::from(pad));
    image::DynamicImage::ImageRgba8(canvas)
}

/// Render a LaTeX display-math source to an SVG string, wrapping any panic
/// in a [`DiagramError`] (RaTeX 0.1.x can panic on pathological input —
/// same defence as the mermaid renderer).  Enforces the same
/// [`MAX_LATEX_SOURCE_BYTES`] cap as [`resolve_latex`].
///
/// This is the shared entry point for both the TUI raster path (via
/// [`resolve_latex`]) and the HTML exporter, which rasterizes the returned
/// SVG to a PNG rather than inlining it — the exact parallel to
/// [`super::mermaid::render_mermaid_svg`].
///
/// * `fg` — glyph colour as RGBA.  The TUI passes the theme's text colour;
///   the exporter passes opaque black for a light document background.
/// * `font_size` — the terminal cell's `(width, height)` in pixels; the
///   cell *height* drives RaTeX's `font_size` through [`LATEX_EM_TO_CELL`]
///   so the formula's x-height matches the surrounding body text.  `None`
///   (the exporter's case) falls back to a 16 px cell.
pub fn render_latex_svg(
    source: &str,
    fg: [u8; 4],
    font_size: Option<(u16, u16)>,
) -> Result<String, DiagramError> {
    if source.len() > MAX_LATEX_SOURCE_BYTES {
        return Err(DiagramError::RenderFailed(format!(
            "latex source too large: {} bytes (max {MAX_LATEX_SOURCE_BYTES})",
            source.len()
        )));
    }
    let outcome = {
        let _expected = crate::terminal::ExpectedPanic::new();
        catch_unwind(AssertUnwindSafe(|| {
            render_latex_svg_inner(source, fg, font_size)
        }))
    }
    .map_err(|payload| {
        DiagramError::RenderFailed(format!("latex render panic: {}", panic_message(&payload)))
    })?;
    outcome.map_err(|e| DiagramError::RenderFailed(format!("{e:#}")))
}

/// Unwrapped RaTeX pipeline: parse → layout (display style) → display list
/// → `<text>` SVG.
fn render_latex_svg_inner(
    source: &str,
    fg: [u8; 4],
    font_size: Option<(u16, u16)>,
) -> Result<String, ratex_parser::error::ParseError> {
    use ratex_layout::layout_options::LayoutOptions;
    use ratex_layout::{layout, to_display_list};
    use ratex_parser::parse;
    use ratex_svg::{render_to_svg, SvgOptions};
    use ratex_types::color::Color;

    let ast = parse(source)?;
    let [r, g, b, a] = fg;
    let options = LayoutOptions::default().with_color(Color::new(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        f32::from(a) / 255.0,
    ));
    let lbox = layout(&ast, &options);
    let display_list = to_display_list(&lbox);
    // One RaTeX em per ~0.6 cell-height pixel (see `LATEX_EM_TO_CELL`): the
    // formula's x-height then matches the surrounding body text.  Fall back
    // to a 16 px cell (a common terminal default) when unknown.
    let em = font_size.map_or(16.0, |(_, h)| f64::from(h.max(1))) * LATEX_EM_TO_CELL;
    // Text output, not embedded glyph outlines: `embed_glyphs = false`
    // emits `<text font-family="KaTeX_*">`, and the shared fontdb (which
    // carries the bundled KaTeX faces, see `image::svg`) resolves those
    // families during the rasterize step — exactly how the mermaid path
    // renders.  Single font subsystem; no standalone/embed-fonts features
    // and their parallel ab_glyph stack.
    let svg = render_to_svg(
        &display_list,
        &SvgOptions {
            embed_glyphs: false,
            font_size: em,
            // RaTeX's default padding (a fixed 10 user units per side) is a
            // large, scale-invariant frame next to body-sized text.  We
            // replace it with an em-relative margin (`LATEX_PADDING_EMS`)
            // so glyphs keep a small, proportional gap from the image edge
            // — the padding the HTML export and the in-app raster share.
            // The breathing-room step adds the extra text-line gap on top.
            padding: em * LATEX_PADDING_EMS,
            ..SvgOptions::default()
        },
    );
    Ok(svg)
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    /// The measurement is the whole point of this half: the source's own width is the wrong
    /// number — the delimiters and an ASCII transcription like `Y_1` say nothing about the ink.
    #[test]
    fn the_measured_width_is_the_ink_not_the_source() {
        let cell = Some((10u16, 20u16));
        for (source, source_cells) in [("Y_1", 5usize), ("\\alpha", 7), ("\\frac{1}{2}", 11)] {
            let cells = inline_latex_width_cells(source, cell, 0.74).expect("layouts") as usize;
            assert!(cells >= 1, "{source:?}");
            assert!(
                cells < source_cells,
                "{source:?}: {cells} cells of ink must be narrower than the {source_cells} source characters"
            );
        }
        // Width follows the *raster's* ink, not the source and not the natural ink either: the
        // one-row budget trades height for width, so a `\frac` (deep, therefore squeezed hard)
        // measures *narrower* than a single letter while its source is far longer.  That is the
        // squeeze, stated as a number.
        let letter = inline_latex_width_cells("x", cell, 0.74).expect("layouts");
        let wide = inline_latex_width_cells("\\sum_{i=1}^{n} x_i", cell, 0.74).expect("layouts");
        assert!(wide > letter, "wide {wide} must exceed letter {letter}");
        // A source RaTeX refuses yields no width, so the caller keeps the literal source.
        assert!(inline_latex_width_cells("\\notacommand{", cell, 0.74).is_none());
    }

    /// An inline formula is set at *body* size: the room constraints alone let a short formula
    /// double its em, which rendered lone letters noticeably larger than the prose around them.
    /// Measured on the raster: a lowercase `x`'s ink must stay within the x-height a terminal font
    /// gives body text (≈ 0.35 of the cell), not fill the cell's ascent.
    #[test]
    fn an_inline_formula_is_set_at_body_size() {
        let cell = (10u16, 20u16);
        let fg = [230, 230, 230, 255];
        let bg = [26, 26, 26, 255];
        for source in ["x", "a", "c"] {
            let img = render_inline_latex(source, 3, Some(cell), fg, bg, 0.74).expect("renders");
            let rgba = img.to_rgba8();
            let ink_rows: Vec<u32> = (0..rgba.height())
                .filter(|&y| (0..rgba.width()).any(|x| rgba.get_pixel(x, y).0 != bg))
                .collect();
            let height =
                ink_rows.last().copied().unwrap_or(0) - ink_rows.first().copied().unwrap_or(0) + 1;
            assert!(
                height <= 10,
                "{source:?}: the ink is {height} px of a 20 px cell — a lowercase letter must not \
                 exceed the body text's x-height"
            );
        }
    }

    /// The one claim the whole spike rests on: the formula's ink bottom — its baseline, for a
    /// glyph without a descender — lands on `baseline_ratio · cell_h`, which is where the text's
    /// own baseline sits.  Asserted on the raster, where the answer is a number rather than an
    /// impression.
    #[test]
    fn the_ink_bottom_lands_on_the_baseline() {
        let cell = (10u16, 20u16);
        let fg = [230, 230, 230, 255];
        // The bitmap is opaque on the page colour, so "ink" is a pixel that differs from it.
        let bg = [26, 26, 26, 255];
        let ink_bottom = |source: &str, ratio: f32| -> u32 {
            let img = render_inline_latex(source, 3, Some(cell), fg, bg, ratio).expect("renders");
            assert_eq!(img.width(), 30, "the bitmap is the atom's cell box");
            assert_eq!(img.height(), 20, "exactly one cell row tall");
            let rgba = img.to_rgba8();
            (0..rgba.height())
                .rev()
                .find(|&y| (0..rgba.width()).any(|x| rgba.get_pixel(x, y).0 != bg))
                .expect("the formula has ink")
        };

        // `X` has no descender: its ink bottom *is* the baseline.
        for ratio in [0.7f32, 0.8, 0.9] {
            let want = (ratio * 20.0).round() as i64;
            let got = i64::from(ink_bottom("X", ratio));
            assert!(
                (got - want).abs() <= 1,
                "ratio {ratio}: ink bottom {got} must land on the baseline {want}"
            );
        }

        // A subscript descends below it, and by less than a cell — the sub-cell offset a
        // cell-aligned placement cannot express, which is what the padded raster is for.
        let bottom = ink_bottom("Y_1", 0.8);
        assert!(
            (16..20).contains(&bottom),
            "Y_1's descender must stay inside its own cell row, got {bottom}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Display math must rasterize to a real image through the same
    /// `LoadedImage` contract as mermaid.  The KaTeX faces are bundled
    /// into the shared fontdb (`image::svg`), so this needs no system
    /// fonts and runs in CI — the render path's regression guard.
    #[test]
    fn latex_display_math_renders_to_loaded_image() {
        let loaded = resolve_latex(
            "test".into(),
            r"x^2 + y^2 = z^2",
            Some((80, 24)),
            Some((8, 16)),
            [0xcc, 0xcc, 0xcc, 255],
            [0x1a, 0x1a, 0x1a, 255],
        )
        .expect("trivial display math should render");
        assert!(loaded.image.width() > 0);
        assert!(loaded.image.height() > 0);
    }

    /// The rendered formula's pixel height must track the terminal cell
    /// font size (16 px cell → roughly one text line), not balloon to the
    /// whole image envelope — the bug where display math rendered huge.
    /// Bundled fonts → runs in CI.
    #[test]
    fn latex_image_height_tracks_cell_font_size() {
        // Envelope is 40 cells tall but a single-line formula must come
        // out near one cell (16 px) tall — Natural mode, no fill.
        let loaded = resolve_latex(
            "test".into(),
            r"x^2 + y^2 = z^2",
            Some((80, 40)),
            Some((8, 16)),
            [0xcc, 0xcc, 0xcc, 255],
            [0x1a, 0x1a, 0x1a, 255],
        )
        .expect("display math should render");
        // Natural-mode raster keeps the SVG's own size: with the em bump
        // (`LATEX_EM_TO_CELL`) and the em-relative internal padding
        // (`LATEX_PADDING_EMS`), a single line of math at a 16 px cell
        // rasterizes to a few tens of pixels and then rounds up to a whole
        // number of cells — a couple of cells tall, nowhere near the 640 px
        // a full-envelope fill of the 40-cell envelope would produce.
        let h = loaded.image.height();
        assert!(
            (16..=64).contains(&h),
            "single-line formula should be a few cells tall, got {h}px"
        );
    }

    /// The breathing-room pad scales with the reported cell height and
    /// keeps the formula's pixels centred vertically inside it (no
    /// content shift, only transparent margin added top and bottom).
    #[test]
    fn breathing_room_pads_transparent_rows_above_and_below() {
        use image::GenericImageView;
        use image::Rgba;
        // 4×8 solid-red image, cell height 30 → pad 3 rows each side.
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            4,
            8,
            Rgba([200, 0, 0, 255]),
        ));
        let padded = add_formula_breathing_room(img, Some((8, 30)));
        assert_eq!(padded.dimensions(), (4, 8 + 2 * 3));
        let rgba = padded.to_rgba8();
        // Top pad rows transparent, first content row red.
        assert_eq!(rgba.get_pixel(0, 0).0[3], 0);
        assert_eq!(rgba.get_pixel(0, 2).0[3], 0);
        assert_eq!(rgba.get_pixel(0, 3).0, [200, 0, 0, 255]);
        // Bottom pad rows transparent, last content row red.
        // content 3..11, bottom pad 11..14 (height 8+2*3).
        assert_eq!(rgba.get_pixel(0, 8 + 3).0[3], 0); // first bottom pad
        assert_eq!(rgba.get_pixel(0, 8 + 2 * 3 - 1).0[3], 0); // last row
        assert_eq!(rgba.get_pixel(0, 8 + 3 - 1).0, [200, 0, 0, 255]); // last content
    }

    /// Unknown cell size falls back to a 16 px cell (pad 1).
    #[test]
    fn breathing_room_falls_back_to_a_default_cell_height() {
        let img = image::DynamicImage::new_rgba8(2, 2);
        let padded = add_formula_breathing_room(img, None);
        assert_eq!(padded.height(), 2 + 2);
    }

    /// Transparent formula pixels must flatten onto the document
    /// background, never survive as black — halfblocks encode via
    /// `to_rgb8()`, which reads a transparent pixel's RGB as-is
    /// (`Rgba([0,0,0,0])` → black).
    #[test]
    fn flatten_composites_transparency_onto_the_document_background() {
        use image::{GenericImageView, Rgba};
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(2, 1, |x, _| {
            if x == 0 {
                Rgba([0, 0, 0, 0])
            } else {
                Rgba([200, 0, 0, 255])
            }
        }));
        let out = flatten_to_background(img, [10, 20, 30, 255]);
        let rgba = out.to_rgba8();
        assert_eq!(
            rgba.get_pixel(0, 0).0,
            [10, 20, 30, 255],
            "transparent → bg"
        );
        assert_eq!(
            rgba.get_pixel(1, 0).0,
            [200, 0, 0, 255],
            "opaque content kept"
        );
        assert_eq!(rgba.get_pixel(0, 0).0[3], 255, "output fully opaque");
        assert_eq!(out.dimensions(), (2, 1), "dimensions unchanged");
    }

    /// Rounding slack between a formula's pixel height and the reserved
    /// whole-cell rows must split above AND below the content (vertical
    /// centring) — never all below, which would read as a gap only under
    /// the formula.
    #[test]
    fn cell_grid_centring_splits_rounding_slack_evenly() {
        use image::{GenericImageView, Rgba};
        // 2×10 opaque red; cell height 30 → ceil(10/30)=1 row = 30px →
        // extra 20px, 10 above and 10 below.
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            2,
            10,
            Rgba([200, 0, 0, 255]),
        ));
        let out = center_on_cell_grid(img, Some((8, 30)), [10, 20, 30, 255]);
        assert_eq!(out.dimensions(), (2, 30));
        let rgba = out.to_rgba8();
        assert_eq!(rgba.get_pixel(0, 0).0, [10, 20, 30, 255], "top pad = bg");
        assert_eq!(rgba.get_pixel(0, 9).0, [10, 20, 30, 255], "top half slack");
        assert_eq!(
            rgba.get_pixel(0, 10).0,
            [200, 0, 0, 255],
            "content starts at 10"
        );
        assert_eq!(
            rgba.get_pixel(0, 19).0,
            [200, 0, 0, 255],
            "content ends at 19"
        );
        assert_eq!(
            rgba.get_pixel(0, 20).0,
            [10, 20, 30, 255],
            "bottom pad = bg"
        );
        assert_eq!(
            rgba.get_pixel(0, 29).0,
            [10, 20, 30, 255],
            "bottom half slack"
        );
    }

    /// An image that already fills its cells exactly is untouched.
    #[test]
    fn cell_grid_centring_is_a_noop_on_exact_multiples() {
        let img = image::DynamicImage::new_rgba8(4, 60); // 2 rows of 30
        let out = center_on_cell_grid(img, Some((8, 30)), [0, 0, 0, 255]);
        assert_eq!(out.height(), 60);
    }
}
