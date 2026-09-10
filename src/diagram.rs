//! Diagram and display-math rendering: fenced ```mermaid``` blocks and `$$...$$` paragraphs
//! become images that flow through the image pipeline under synthetic content-addressed URLs
//! (`diagram-mermaid-<sha256>` / `diagram-math-<sha256>`), so the cache reuses renders across
//! reparses and any edit to a block yields a fresh render.
//!
//! Backends live in [`mermaid`] and [`math`]; [`common`] holds the shared `DiagramSource`
//! discriminator (carried on `ImageBlockInfo.source`, which the App's decode dispatcher branches
//! on), the `DiagramError` type, and the synthetic-URL scheme.

pub mod common;
pub mod math;
pub mod mermaid;

// rustc misreports `render_mermaid_svg` as unused on this re-export (see `src/config.rs`).
#[allow(unused_imports)]
pub use common::{is_diagram_url, synthetic_url, DiagramSource};
#[allow(unused_imports)]
pub use math::{render_latex_svg, resolve_latex};
#[allow(unused_imports)]
pub use mermaid::{render_mermaid_svg, resolve_mermaid, warm_fontdb};
