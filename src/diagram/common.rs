//! Shared plumbing for the diagram backends (mermaid, math).
//!
//! Holds the pieces both renderers need: the [`DiagramSource`] discriminator
//! carried on `ImageBlockInfo.source`, the [`DiagramError`] type shipped back
//! through the App's mpsc channel, the synthetic cache-key URL scheme, and
//! the `catch_unwind` payload helper.  The backends themselves live in
//! [`super::mermaid`] and [`super::math`].
//!
//! The synthetic-URL format is `<prefix><lowercase-hex-sha256>` — stable
//! across reparses so the image cache reuses renders, and content-addressed
//! so editing inside a block invalidates only that block's entry.  The URL is
//! opaque to every other part of the system; `ImageBlockInfo.source` is the
//! reliable discriminator.

use std::fmt::Write;

use sha2::{Digest, Sha256};

use crate::image::SvgError;

/// Source for a diagram block.  The enum exists so future backends
/// (PlantUML, Graphviz/DOT, D2) can be added without rewiring
/// `ImageBlockInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DiagramSource {
    Mermaid(String),
    /// LaTeX display math promoted from a `$$...$$`-only paragraph.
    Latex(String),
}

/// Errors reported by the diagram pipeline.  The renderer / rasterizer /
/// decoder each have their own variant so the hint line can surface a
/// specific failure mode.  The variants carry owned `String` messages
/// rather than source-chained errors so `DiagramError` stays `Send + Sync`
/// and can be shipped back through the App's mpsc channel.
#[derive(Debug, thiserror::Error)]
pub enum DiagramError {
    #[error("render failed: {0}")]
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

/// Best-effort extraction of a message from a `catch_unwind` payload.
/// Panics in Rust are usually `String` or `&'static str`; anything else
/// falls back to a generic marker so the cache entry still reports a
/// failure.  Shared by both backends' panic guards.
pub(crate) fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
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
    use super::*;

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
    fn synthetic_url_differs_for_different_sources() {
        let a = synthetic_url(&DiagramSource::Mermaid("flowchart TD\nA-->B".into()));
        let b = synthetic_url(&DiagramSource::Mermaid("flowchart TD\nA-->C".into()));
        assert_ne!(a, b);
    }
}
