//! The user manual (`docs/*.md`), `include_str!`d at build time and opened as pathless,
//! read-only in-memory documents (see [`crate::app::App::open_doc_page`]). Embedded rather
//! than extracted to disk so the manual can never drift from the build it documents and
//! there is no cache directory to manage. This module parses nothing — it owns static
//! strings and slug metadata only, which is what keeps it a leaf. See `docs/dev/in-app-docs.md`.

pub mod link;
pub mod registry;

pub use link::{resolve_doc_reference, DocLinkResolution};
pub use registry::{DocId, DocPage, ALL_DOCS};
