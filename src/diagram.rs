//! Diagram and display-math rendering.
//!
//! Turns fenced ```mermaid``` code blocks and `$$...$$` display-math
//! paragraphs into images that flow through the image pipeline (AST
//! `Block::ImageBlock` → decode worker → URL-keyed `ImageCache` →
//! per-frame overlay).  Each gets a synthetic content-addressed URL
//! (`diagram-mermaid-<sha256>` / `diagram-math-<sha256>`) so the cache
//! reuses renders across reparses while a keystroke inside a block
//! produces a new hash → a fresh render.
//!
//! The backends live in [`mermaid`] and [`math`]; [`common`] holds the
//! shared `DiagramSource` discriminator (carried on `ImageBlockInfo.source`,
//! which the App's decode dispatcher branches on to pick the right worker),
//! the `DiagramError` type, and the synthetic-URL scheme.

pub mod common;
pub mod math;
pub mod mermaid;

// `render_mermaid_svg` is consumed via `crate::diagram::` from `src/export/`,
// but rustc misreports it as unused on this re-export. See similar note in
// `src/config.rs`.
#[allow(unused_imports)]
pub use common::{is_diagram_url, synthetic_url, DiagramSource};
#[allow(unused_imports)]
pub use math::{render_latex_svg, resolve_latex};
#[allow(unused_imports)]
pub use mermaid::{render_mermaid_svg, resolve_mermaid, warm_fontdb};
