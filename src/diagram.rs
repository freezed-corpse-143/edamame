//! Diagram rendering: fenced ```mermaid``` blocks become images that flow through the
//! image pipeline under a synthetic `diagram-mermaid-<sha256(source)>` URL, so the cache
//! reuses renders across reparses and any edit to the block yields a fresh render.

pub mod mermaid;

// rustc misreports `render_mermaid_svg` as unused on this re-export (see `src/config.rs`).
#[allow(unused_imports)]
pub use mermaid::{
    is_diagram_url, render_mermaid_svg, resolve_mermaid, synthetic_url, warm_fontdb, DiagramSource,
};
