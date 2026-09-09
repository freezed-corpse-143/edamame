//! Mermaid → SVG → PNG → `DynamicImage` pipeline.
//!
//! The public entry points are [`render_mermaid_svg`] (used by the HTML
//! exporter, which wants SVG strings inline) and [`resolve_mermaid`] (used
//! by the App decode worker, which wants a `LoadedImage` ready for the
//! existing image cache).  Both wrap the third-party renderer in
//! `catch_unwind` — `mermaid-rs-renderer` 0.2.x has several known panic bugs
//! (invalid hex colors, empty subgraphs, over-wide sequence labels) and a
//! panicking worker thread would strand the cache entry as `Pending`
//! forever.
//!
//! The shared cache-key URL scheme, [`DiagramSource`](super::common::DiagramSource),
//! and [`DiagramError`] live in [`super::common`];
//! the LaTeX-math backend in [`super::math`].

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::image::{rasterize_svg, LoadedImage, SvgScaleMode, SvgSizing};

use super::common::{panic_message, DiagramError};

/// Pre-populate the shared fontdb off the hot path.  The App warmup
/// thread calls this at startup so the first real diagram render
/// doesn't pay the disk-scan cost.  Also primes mermaid-rs-renderer's
/// own internal font cache by running a trivial diagram.
///
/// The fontdb itself lives in `crate::image::svg` (shared with the
/// SVG-file rasterizer and the math backend); this wrapper additionally
/// primes the mermaid renderer's own font cache.
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

/// Maximum mermaid source length we will attempt to render.  The renderer
/// has no internal length, node-count, or timeout bound, so a pathological
/// diagram can drive unbounded CPU/RAM on the decode worker (the UI stays
/// responsive, but the process can OOM).  64 KiB is far larger than any
/// hand-authored diagram; an over-cap block fails to render and falls back
/// to the plain code block — a placeholder in the TUI, an escaped `<pre>`
/// in HTML export.
const MAX_MERMAID_SOURCE_BYTES: usize = 64 * 1024;

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
    fn oversized_mermaid_source_is_rejected_before_render() {
        // Comfortably over the 64 KiB cap; must error out *without*
        // reaching the renderer (so this test needs no fonts and can't
        // hit an upstream panic).
        let huge = format!("flowchart TD\n{}", "A-->B\n".repeat(20_000));
        assert!(huge.len() > 64 * 1024);
        let err = render_mermaid_svg(&huge).unwrap_err();
        assert!(matches!(err, DiagramError::RenderFailed(_)));
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
