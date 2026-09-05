//! Mermaid → SVG → PNG → `DynamicImage` pipeline.  [`render_mermaid_svg`] serves the HTML
//! exporter; [`resolve_mermaid`] serves the App decode worker.
//!
//! Every call into the third-party renderer is wrapped in `catch_unwind`: `mermaid-rs-renderer`
//! has known panic bugs (invalid hex colors, empty subgraphs, over-wide sequence labels) and a
//! panicking worker thread would strand the cache entry as `Pending` forever.
//!
//! The synthetic URL `diagram-mermaid-<hex-sha256>` is content-addressed, so it is stable across
//! reparses and editing one block invalidates only that block.  It is opaque elsewhere;
//! `ImageBlockInfo.source` is the reliable discriminator.

use std::fmt::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};

use sha2::{Digest, Sha256};

use crate::image::{rasterize_svg, LoadedImage, SvgError, SvgScaleMode, SvgSizing};

/// Pre-populate the shared fontdb (which lives in `crate::image::svg`) and mermaid-rs-renderer's
/// own font cache, off the hot path.  Called by the App warmup thread at startup.
pub fn warm_fontdb() {
    crate::image::svg::warm_fontdb();
    // Best-effort, so a known upstream panic must not escape.  The guard keeps the process panic
    // hook from restoring the terminal out from under the running TUI; the diagram is a literal,
    // so here the hazard is the hook, not the input.  See `terminal::panic_guard`.
    let _expected = crate::terminal::ExpectedPanic::new();
    let _ = catch_unwind(|| {
        let _ = mermaid_rs_renderer::render("flowchart TD\nA-->B\n");
    });
}

/// Source for a diagram block.  An enum so other backends can be added without rewiring
/// `ImageBlockInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DiagramSource {
    Mermaid(String),
}

/// Errors reported by the diagram pipeline, one variant per stage so the hint line can name the
/// failure.  Messages are owned `String`s rather than chained sources so this stays `Send + Sync`
/// for the App's mpsc channel.
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

impl From<SvgError> for DiagramError {
    fn from(err: SvgError) -> Self {
        match err {
            SvgError::Parse(m) => DiagramError::SvgParse(m),
            SvgError::Raster(m) => DiagramError::Raster(m),
            SvgError::Decode(m) => DiagramError::Decode(m),
        }
    }
}

/// Prefix shared by every URL produced by [`synthetic_url`]; see [`is_diagram_url`].
const SYNTHETIC_URL_PREFIX: &str = "diagram-mermaid-";

/// Synthetic cache-key URL for a mermaid source; stable across process invocations.
pub fn synthetic_url(source: &DiagramSource) -> String {
    match source {
        DiagramSource::Mermaid(src) => {
            let digest = Sha256::digest(src.as_bytes());
            let mut hex = String::with_capacity(digest.len() * 2);
            for byte in digest {
                write!(hex, "{byte:02x}").expect("writing to a String is infallible");
            }
            format!("{SYNTHETIC_URL_PREFIX}{hex}")
        }
    }
}

/// True for a [`synthetic_url`] key, as opposed to a document-authored image URL.
pub fn is_diagram_url(url: &str) -> bool {
    url.starts_with(SYNTHETIC_URL_PREFIX)
}

/// The renderer has no internal length, node-count, or timeout bound, so a pathological diagram
/// can drive the decode worker to OOM.  An over-cap block falls back to the plain code block.
const MAX_MERMAID_SOURCE_BYTES: usize = 64 * 1024;

/// Render a mermaid source to SVG, wrapping any panic or error in a [`DiagramError`].
pub fn render_mermaid_svg(source: &str) -> Result<String, DiagramError> {
    if source.len() > MAX_MERMAID_SOURCE_BYTES {
        return Err(DiagramError::RenderFailed(format!(
            "mermaid source too large: {} bytes (max {MAX_MERMAID_SOURCE_BYTES})",
            source.len()
        )));
    }
    // Tells the process panic hook this one is caught, so it neither restores the terminal out
    // from under a running TUI nor prints the payload through it.  Scoped to the `catch_unwind`
    // alone: a guard still live afterwards would silence a panic nobody catches.
    let outcome = {
        let _expected = crate::terminal::ExpectedPanic::new();
        catch_unwind(AssertUnwindSafe(|| mermaid_rs_renderer::render(source)))
    }
    .map_err(|payload| DiagramError::RenderFailed(format!("panic: {}", panic_message(&payload))))?;
    outcome.map_err(|e| DiagramError::RenderFailed(format!("{e:#}")))
}

/// Render a mermaid source all the way to a `LoadedImage` for the image cache.
///
/// * `url` — the synthetic cache key the caller already computed; carried on the result so the
///   main-thread lookup resolves to the right entry.
/// * `max_cells` / `font_size` — target cell envelope, converted to pixels so the pixmap is never
///   larger than the terminal can display.  `None` keeps the SVG's natural resolution.
pub fn resolve_mermaid(
    url: String,
    source: &str,
    max_cells: Option<(u16, u16)>,
    font_size: Option<(u16, u16)>,
) -> Result<LoadedImage, DiagramError> {
    let svg = render_mermaid_svg(source)?;
    // Diagrams have no meaningful natural size, so fill the envelope either way.  White
    // background because mermaid SVGs are transparent but meant to be read on a light page.
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

/// Best-effort message from a `catch_unwind` payload; an unrecognized payload still reports a
/// failure rather than being lost.
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

    // Compile-time check: the render result must be `Send` for the decode worker.
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
        assert_eq!(a.len(), "diagram-mermaid-".len() + 64);
    }

    #[test]
    fn oversized_mermaid_source_is_rejected_before_render() {
        // Must error out *without* reaching the renderer, so this test needs no fonts.
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

    // Non-deterministic across font installs, so this is a "does it render at all" check only.
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

    // Envelope scaling is exercised in `crate::image::svg`, where `rasterize_svg` lives.

    // Counterfactual for `mermaid_live_throughput` below: per-render cost when each call does its
    // own `load_system_fonts()`, the path the shared fontdb replaced.
    #[test]
    #[ignore = "requires system fonts; counterfactual benchmark only"]
    fn mermaid_live_throughput_unshared_fontdb() {
        // A fresh SVG parse per call with its own fontdb, mirroring the pre-fix path.
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
                // Render, then rasterize with an *unshared* fontdb.
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

    // Hot-loop benchmark.  With the shared fontdb the per-iteration cost stays constant; before
    // it, each iteration paid a fresh `load_system_fonts` (~100–300 ms).
    #[test]
    #[ignore = "requires system fonts; exercises live mermaid-rs-renderer"]
    fn mermaid_live_throughput() {
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

    // Canary: malformed mermaid input must yield an `Err`, never unwind out of the closure.
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
            // Either variant is acceptable; not unwinding is the point.
            let _ = result;
        }
    }
}
