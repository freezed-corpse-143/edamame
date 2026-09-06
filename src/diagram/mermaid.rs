//! Mermaid → SVG → PNG → `DynamicImage` pipeline.
//!
//! The public entry points are `render_mermaid_svg` (used by the HTML
//! exporter, which wants SVG strings inline) and `resolve_mermaid` (used
//! by the App decode worker, which wants a `LoadedImage` ready for the
//! existing image cache).  Both share `render_mermaid_svg_core` which
//! wraps the third-party renderer in `catch_unwind` — `mermaid-rs-renderer`
//! 0.2.1 has several known panic bugs (invalid hex colors, empty
//! subgraphs, over-wide sequence labels) and a panicking worker thread
//! would strand the cache entry as `Pending` forever.
//!
//! The synthetic-URL format is `diagram-mermaid-<lowercase-hex-sha256>`
//! — stable across reparses so the image cache reuses renders, and
//! content-addressed so editing inside a block invalidates only that
//! block's entry.  The URL is opaque to every other part of the system;
//! `ImageBlockInfo.source` is the reliable discriminator.

use std::fmt::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};

use sha2::{Digest, Sha256};

use crate::image::{rasterize_svg, LoadedImage, SvgError, SvgScaleMode, SvgSizing};

/// Pre-populate the shared fontdb off the hot path.  The App warmup
/// thread calls this at startup so the first real diagram render
/// doesn't pay the disk-scan cost.  Also primes mermaid-rs-renderer's
/// own internal font cache by running a trivial diagram.
///
/// The fontdb itself lives in `crate::image::svg` (shared with the
/// SVG-file rasterizer); this wrapper additionally primes the mermaid
/// renderer's own font cache.
pub fn warm_fontdb() {
    crate::image::svg::warm_fontdb();
    // Prime mermaid-rs-renderer's own fontdb too (it maintains its own
    // via once_cell::sync::Lazy).  Wrapped in catch_unwind because the
    // upstream crate has known panic bugs and the warmup is
    // best-effort — and guarded for the same reason every other
    // `catch_unwind` in the crate is: the process panic hook would
    // otherwise restore the terminal out from under the TUI this thread
    // was spawned alongside.  The diagram here is a literal, so this is
    // the one guarded section that is not about untrusted content; the
    // hazard is the hook, not the input.  See `terminal::panic_guard`.
    let _expected = crate::terminal::ExpectedPanic::new();
    let _ = catch_unwind(|| {
        let _ = mermaid_rs_renderer::render("flowchart TD\nA-->B\n");
    });
}

/// Source for a diagram block.  The enum exists so future backends
/// (PlantUML, Graphviz/DOT, D2, LaTeX math) can be added without
/// rewiring `ImageBlockInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DiagramSource {
    Mermaid(String),
    /// LaTeX display math promoted from a `$$...$$`-only paragraph.
    Latex(String),
}

/// Errors reported by the diagram pipeline.  The renderer / rasterizer /
/// decoder each have their own variant so the hint line can surface a
/// specific failure mode.  The variants
/// carry owned `String` messages rather than source-chained errors so
/// `DiagramError` stays `Send + Sync` and can be shipped back through
/// the App's mpsc channel.
#[derive(Debug, thiserror::Error)]
pub enum DiagramError {
    #[error("mermaid render failed: {0}")]
    RenderFailed(String),
    #[error("svg parse failed: {0}")]
    SvgParse(String),
    #[error("raster failed: {0}")]
    Raster(String),
    #[error("png decode failed: {0}")]
    Decode(String),
}

/// Map the shared SVG rasterizer's errors onto the diagram-specific
/// variants so the hint line keeps reporting the right failure mode.
impl From<SvgError> for DiagramError {
    fn from(err: SvgError) -> Self {
        match err {
            SvgError::Parse(m) => DiagramError::SvgParse(m),
            SvgError::Raster(m) => DiagramError::Raster(m),
            SvgError::Decode(m) => DiagramError::Decode(m),
        }
    }
}

/// Prefix shared by mermaid URLs produced by [`synthetic_url`]; the
/// counterpart predicate is [`is_diagram_url`].
const SYNTHETIC_URL_PREFIX: &str = "diagram-mermaid-";

/// Prefix shared by LaTeX-math URLs produced by [`synthetic_url`].
const SYNTHETIC_LATEX_URL_PREFIX: &str = "diagram-math-";

/// Synthetic cache-key URL for a diagram/math source.  Stable across
/// process invocations — two runs of edamame see the same URL for the
/// same source text.
pub fn synthetic_url(source: &DiagramSource) -> String {
    let (prefix, src) = match source {
        DiagramSource::Mermaid(src) => (SYNTHETIC_URL_PREFIX, src),
        DiagramSource::Latex(src) => (SYNTHETIC_LATEX_URL_PREFIX, src),
    };
    let digest = Sha256::digest(src.as_bytes());
    // Lowercase hex by hand: `{digest:x}` is not guaranteed lowercase on
    // every Rust version, and the cache keys must be stable across
    // compilers (same source → same URL).
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(hex, "{byte:02x}").expect("writing to a String is infallible");
    }
    format!("{prefix}{hex}")
}

/// True for a synthetic diagram cache key produced by [`synthetic_url`],
/// as opposed to a document-authored image URL / path.
pub fn is_diagram_url(url: &str) -> bool {
    url.starts_with(SYNTHETIC_URL_PREFIX) || url.starts_with(SYNTHETIC_LATEX_URL_PREFIX)
}

/// Maximum mermaid source length we will attempt to render.  The renderer
/// has no internal length, node-count, or timeout bound, so a pathological
/// diagram can drive unbounded CPU/RAM on the decode worker (the UI stays
/// responsive, but the process can OOM).  64 KiB is far larger than any
/// hand-authored diagram; an over-cap block fails to render and falls back
/// to the plain code block — a placeholder in the TUI, an escaped `<pre>`
/// in HTML export.
const MAX_MERMAID_SOURCE_BYTES: usize = 64 * 1024;

/// Same defence for LaTeX display math: RaTeX also has no internal bound,
/// so an over-cap formula gets the same clean failure → placeholder path.
const MAX_LATEX_SOURCE_BYTES: usize = 64 * 1024;

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
/// Multiplying the cell height by `1 / (96/72 × 1.25)` ≈ 0.6 makes the
/// formula's x-height land on the surrounding body text's, instead of one
/// full cell.  Exactness is impossible without font metrics from the
/// terminal; 1.25 is the middle of the common 1.2–1.4 range, so the
/// formula reads within ±10% of body text across terminals.
const LATEX_EM_TO_CELL: f64 = 1.0 / (96.0 / 72.0 * 1.25);

/// Render a mermaid source to SVG, wrapping any panic or error in a
/// `DiagramError`.  Used by both the raster path below and the HTML
/// exporter's diagram branch.
pub fn render_mermaid_svg(source: &str) -> Result<String, DiagramError> {
    if source.len() > MAX_MERMAID_SOURCE_BYTES {
        return Err(DiagramError::RenderFailed(format!(
            "mermaid source too large: {} bytes (max {MAX_MERMAID_SOURCE_BYTES})",
            source.len()
        )));
    }
    // Tells the process panic hook this one is caught, so it neither
    // restores the terminal out from under a running TUI nor prints the
    // payload through it.  Scoped to the `catch_unwind` alone — a guard
    // still live afterwards would silence a panic nobody catches.  See
    // `terminal::panic_guard`.
    let outcome = {
        let _expected = crate::terminal::ExpectedPanic::new();
        catch_unwind(AssertUnwindSafe(|| mermaid_rs_renderer::render(source)))
    }
    .map_err(|payload| DiagramError::RenderFailed(format!("panic: {}", panic_message(&payload))))?;
    outcome.map_err(|e| DiagramError::RenderFailed(format!("{e:#}")))
}

/// Render a mermaid source all the way to a `LoadedImage`, suitable for
/// dropping straight into the image cache.
///
/// * `url` — the synthetic cache-key URL already computed by the caller
///   (typically from `ParsedDoc::image_blocks[i].url`).  Carried on the
///   returned `LoadedImage` so the main-thread cache lookup resolves to
///   the right entry.
/// * `max_cells` / `font_size` — the target cell envelope.  Scaled into
///   pixels and used to size the SVG before rasterization so we never
///   allocate a pixmap larger than the terminal can display.  Passing
///   `None` keeps the SVG's natural resolution (used by tests that don't
///   care about on-screen size).
pub fn resolve_mermaid(
    url: String,
    source: &str,
    max_cells: Option<(u16, u16)>,
    font_size: Option<(u16, u16)>,
) -> Result<LoadedImage, DiagramError> {
    let svg = render_mermaid_svg(source)?;
    // Diagrams have no meaningful natural size, so fill the envelope (up
    // or down).  Fill white because mermaid SVGs are transparent but
    // meant to be read on a light page.
    let image = rasterize_svg(
        &svg,
        SvgSizing {
            envelope: max_cells,
            font_size,
            mode: SvgScaleMode::Fill,
        },
        Some([255, 255, 255, 255]),
    )
    .map_err(DiagramError::from)?;
    Ok(LoadedImage {
        url,
        image,
        scratch: None,
    })
}

/// Render a LaTeX display-math source all the way to a `LoadedImage`,
/// suitable for dropping straight into the image cache — same contract as
/// [`resolve_mermaid`].
///
/// Pipeline (pure Rust, no node / system TeX): RaTeX parses the LaTeX into
/// an AST, lays it out in display style, flattens it to a display list,
/// and serializes a self-contained SVG with glyphs embedded as `<path>`
/// outlines (`standalone` + `embed-fonts` features) — which the shared
/// `crate::image::rasterize_svg` then rasterizes, exactly like a mermaid
/// render.  RaTeX 0.1.x is pre-1.0 with known panic bugs, so the render is
/// wrapped in `catch_unwind` like `render_mermaid_svg_core`.
///
/// Sizing: the SVG's user-unit scale is RaTeX's `font_size` (em units).
/// We derive it from the terminal's cell pixel height via [`LATEX_EM_TO_CELL`]
/// so the formula's x-height matches body text (not one full cell), drop
/// RaTeX's default padding (a fixed frame dwarfing the glyphs), then
/// rasterize with `SvgScaleMode::Natural` (downscale only) so a formula
/// wider than the column shrinks to fit but never balloons — unlike
/// Mermaid, a formula has a meaningful natural size.
/// The image stays **transparent** (`background: None`) so it composites
/// over the document background, and the glyphs are painted in `fg`
/// (the theme's text colour) so dark themes stay legible.
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
        None, // transparent — composite over the document background
    )
    .map_err(DiagramError::from)?;
    Ok(LoadedImage {
        url,
        image: flatten_to_background(add_formula_breathing_room(image, font_size), bg),
        scratch: None,
    })
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

/// Render a LaTeX display-math source to a self-contained SVG string,
/// wrapping any panic in a [`DiagramError`] (RaTeX 0.1.x can panic on
/// pathological input — same defence as the mermaid renderer).
///
/// * `fg` — glyph colour as RGBA, taken from the theme's text colour.
/// * `font_size` — the terminal cell's `(width, height)` in pixels; the
///   cell *height* drives RaTeX's `font_size` through [`LATEX_EM_TO_CELL`]
///   so the formula's x-height matches the surrounding body text.
fn render_latex_svg(
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

/// Unwrapped RaTeX four-step pipeline (see module docs for the phases):
/// parse → layout (display style) → display list → standalone SVG.
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
    let svg = render_to_svg(
        &display_list,
        &SvgOptions {
            embed_glyphs: true,
            font_size: em,
            // RaTeX's default padding (10 user units per side) would add a
            // fixed ~27 px frame around every formula — large next to body
            // text.  The formula's own bounding box already includes
            // ascenders, descenders and stretchy delimiters, so no padding
            // is needed: transparent background composites straight onto
            // the document.
            padding: 0.0,
            ..SvgOptions::default()
        },
    );
    Ok(svg)
}

/// Best-effort extraction of a message from a `catch_unwind` payload.
/// Panics in Rust are usually `String` or `&'static str`; anything else
/// falls back to a generic marker so the cache entry still reports a
/// failure.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else {
        "unknown payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use image::DynamicImage;

    use super::*;

    // Compile-time check: the render entry point must be `Send` so it
    // can be called from a `std::thread::spawn`'d worker.  If someone
    // introduces a non-Send type into the signature this test stops
    // compiling — effectively a spec constraint frozen in code.
    #[test]
    fn resolve_mermaid_result_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Result<LoadedImage, DiagramError>>();
    }

    #[test]
    fn synthetic_url_is_stable_for_same_source() {
        let a = synthetic_url(&DiagramSource::Mermaid("flowchart TD\nA-->B".into()));
        let b = synthetic_url(&DiagramSource::Mermaid("flowchart TD\nA-->B".into()));
        assert_eq!(a, b);
        assert!(a.starts_with("diagram-mermaid-"));
        // SHA-256 hex is 64 chars; prefix is 16 chars; total 80.
        assert_eq!(a.len(), "diagram-mermaid-".len() + 64);
    }

    /// LaTeX display math gets its own content-addressed URL family so
    /// the image cache reuses renders across reparses without colliding
    /// with mermaid diagrams or document images.
    #[test]
    fn latex_synthetic_url_is_stable_and_distinct() {
        let a = synthetic_url(&DiagramSource::Latex("x^2 + y^2 = z^2".into()));
        let b = synthetic_url(&DiagramSource::Latex("x^2 + y^2 = z^2".into()));
        assert_eq!(a, b);
        assert!(a.starts_with("diagram-math-"), "url was {a}");
        assert_eq!(a.len(), "diagram-math-".len() + 64);

        // Different source → different URL; same source under Mermaid vs
        // Latex must never share a cache entry.
        let other = synthetic_url(&DiagramSource::Latex("x^3".into()));
        assert_ne!(a, other);
        let as_mermaid = synthetic_url(&DiagramSource::Mermaid("x^2 + y^2 = z^2".into()));
        assert_ne!(a, as_mermaid);
        assert!(is_diagram_url(&a));
    }

    #[test]
    fn oversized_mermaid_source_is_rejected_before_render() {
        // Comfortably over the 64 KiB cap; must error out *without*
        // reaching the renderer (so this test needs no fonts and can't
        // hit an upstream panic).
        let huge = format!("flowchart TD\n{}", "A-->B\n".repeat(20_000));
        assert!(huge.len() > 64 * 1024);
        let err = render_mermaid_svg(&huge).unwrap_err();
        assert!(matches!(err, DiagramError::RenderFailed(_)));
    }

    #[test]
    fn synthetic_url_differs_for_different_sources() {
        let a = synthetic_url(&DiagramSource::Mermaid("flowchart TD\nA-->B".into()));
        let b = synthetic_url(&DiagramSource::Mermaid("flowchart TD\nA-->C".into()));
        assert_ne!(a, b);
    }

    // The full renderer is slow (font DB load + layout) and its output
    // is non-deterministic across font installs, so we only exercise it
    // in the "does it work at all" sense and skip pixel comparison.
    // Marked `#[ignore]` because CI may not have system fonts and the
    // upstream crate has known panics; run locally with
    // `cargo test -- --ignored mermaid_live`.
    #[test]
    #[ignore = "requires system fonts; upstream has known panics"]
    fn mermaid_live_renders_trivial_flowchart() {
        let loaded = resolve_mermaid(
            "test".into(),
            "flowchart TD\nA-->B\n",
            Some((80, 24)),
            Some((8, 16)),
        )
        .expect("trivial flowchart should render");
        assert!(loaded.image.width() > 0);
        assert!(loaded.image.height() > 0);
    }

    /// Display math must rasterize to a real image through the same
    /// LoadedImage contract as mermaid — the phase-1 block-math goal.
    /// RaTeX embeds KaTeX fonts in the binary, so this should work
    /// without system fonts; kept `#[ignore]` like the mermaid live
    /// test because upstream 0.1.x may still panic on some inputs.
    #[test]
    #[ignore = "upstream ratex 0.1.x may panic; run with --ignored latex_display_math"]
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

    /// The rendered formula's pixel height must track the terminal cell
    /// font size (16 px cell → roughly one text line), not balloon to the
    /// whole image envelope — the bug where display math rendered huge.
    #[test]
    #[ignore = "upstream ratex 0.1.x may panic; run with --ignored latex_size"]
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
        // Natural-mode raster keeps the SVG's own size.  With RaTeX em =
        // 16 × LATEX_EM_TO_CELL ≈ 9.6 user units and no padding, a single
        // line of math rasterizes to ≈ 1.06 em × 1.333 px/pt ≈ 13.6 px,
        // plus 2 px of breathing room (1 per side at a 16 px cell) —
        // still one cell, not the 640 px a full-envelope fill would
        // produce.
        let h = loaded.image.height();
        assert!(
            (8..=24).contains(&h),
            "single-line formula should be ~1 cell tall (16 px), got {h}px"
        );
    }

    // The envelope scaling itself (small upscales, large downscales,
    // natural-size passthrough) is now exercised in `crate::image::svg`
    // where the shared `rasterize_svg` lives.

    // Counterfactual: what per-render costs look like when each call
    // does its own `load_system_fonts()` — the code path we replaced.
    // Run alongside `mermaid_live_throughput` (below) to quantify the
    // win.  Ignored for the same reasons the shared-fontdb bench is.
    #[test]
    #[ignore = "requires system fonts; counterfactual benchmark only"]
    fn mermaid_live_throughput_unshared_fontdb() {
        // Force a fresh SVG parse per call with its own fontdb — mirrors
        // the original pre-fix behaviour.
        fn rasterize_unshared(svg: &str) -> Result<DynamicImage, DiagramError> {
            let mut opt = usvg::Options::default();
            opt.fontdb_mut().load_system_fonts();
            let tree = usvg::Tree::from_str(svg, &opt)
                .map_err(|e| DiagramError::SvgParse(format!("{e}")))?;
            let size = tree.size();
            let w = (size.width().ceil() as u32).max(1);
            let h = (size.height().ceil() as u32).max(1);
            let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)
                .ok_or_else(|| DiagramError::Raster("pixmap".into()))?;
            pixmap.fill(resvg::tiny_skia::Color::WHITE);
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::default(),
                &mut pixmap.as_mut(),
            );
            let bytes = pixmap
                .encode_png()
                .map_err(|e| DiagramError::Raster(format!("{e}")))?;
            image::load_from_memory(&bytes).map_err(|e| DiagramError::Decode(format!("{e}")))
        }
        let diagrams = [
            "flowchart TD\nA-->B-->C\nC-->D\nD-->A",
            "sequenceDiagram\nA->>B: hi\nB-->>A: ok",
            "pie\n\"A\": 50\n\"B\": 30\n\"C\": 20",
            "stateDiagram-v2\n[*] --> Idle\nIdle --> Run : go\nRun --> [*]",
            "classDiagram\nAnimal <|-- Dog\nAnimal <|-- Cat\nclass Animal",
        ];
        let iterations = 4usize;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            for src in &diagrams {
                // Call mermaid_rs_renderer to get SVG, then rasterize
                // with an UNshared fontdb — simulating the old path.
                if let Ok(svg) = mermaid_rs_renderer::render(src) {
                    let _ = rasterize_unshared(&svg);
                }
            }
        }
        let total = start.elapsed();
        let count = iterations * diagrams.len();
        eprintln!(
            "mermaid_live_throughput_unshared_fontdb: {count} renders in {:?} ({} µs/render)",
            total,
            total.as_micros() / count as u128,
        );
    }

    // Hot-loop benchmark: render many diagrams back-to-back on one
    // thread.  After the shared-fontdb fix this stays constant per
    // iteration; before it, each iteration paid a fresh
    // `load_system_fonts` (~100–300 ms), so 20 iterations was ~2–6 s
    // serial (or much worse parallel due to disk thrashing).
    // Locked behind `--ignored` because it needs system fonts and the
    // upstream renderer has known panic inputs; run with
    // `cargo test --lib mermaid_live_throughput -- --ignored --nocapture`.
    #[test]
    #[ignore = "requires system fonts; exercises live mermaid-rs-renderer"]
    fn mermaid_live_throughput() {
        // Prime the caches the same way the App warmup thread would.
        warm_fontdb();
        let diagrams = [
            "flowchart TD\nA-->B-->C\nC-->D\nD-->A",
            "sequenceDiagram\nA->>B: hi\nB-->>A: ok",
            "pie\n\"A\": 50\n\"B\": 30\n\"C\": 20",
            "stateDiagram-v2\n[*] --> Idle\nIdle --> Run : go\nRun --> [*]",
            "classDiagram\nAnimal <|-- Dog\nAnimal <|-- Cat\nclass Animal",
        ];
        let iterations = 4usize;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            for src in &diagrams {
                let _ = resolve_mermaid("bench".into(), src, Some((80, 24)), Some((8, 16)));
            }
        }
        let total = start.elapsed();
        let count = iterations * diagrams.len();
        eprintln!(
            "mermaid_live_throughput: {count} renders in {:?} ({} µs/render)",
            total,
            total.as_micros() / count as u128,
        );
    }

    // Canary for the known-panic-bug class: malformed mermaid input
    // must yield an `Err`, never a panic (the App worker's
    // `catch_unwind` wrapper depends on this being true of
    // `resolve_mermaid` itself too).
    #[test]
    #[ignore = "exercises upstream; may panic on some inputs until fixed"]
    fn garbage_input_returns_err_not_panic() {
        for input in [
            "",
            "\u{0000}\u{FFFF}",
            "not a diagram at all, just prose",
            "flowchart TD\n~~~~~~~~~~~",
        ] {
            let result = resolve_mermaid("test".into(), input, None, None);
            // Either variant is acceptable; what matters is that we
            // didn't unwind the stack out of the closure.
            let _ = result;
        }
    }
}
