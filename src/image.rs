//! Image loading and caching: the document-layer half of image rendering (URL → decoded
//! `DynamicImage`). The UI half (decoded image → terminal cells) lives in `ui::image_view`.

pub mod cache;
pub mod loader;
pub mod render;
pub mod svg;

// The row-addressed protocol `paint_images` uses for Kitty.  Re-exported so the decode worker
// (which builds it) and the editor view (which paints it) can name it without each reaching into
// `ratatui_image` directly.
pub use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};

// `DecodeStatus` is used by integration tests in tests/editing.rs.
#[allow(unused_imports)]
pub use cache::DecodeStatus;
pub use cache::{
    aspect_rows_of, build_kitty_sliced, render_halfblocks_scratch, ImageCache, NativePaint,
};
pub use loader::{resolve, LoadedImage};
pub use render::paint_halfblocks_partial;
pub use svg::{rasterize_svg, SvgError, SvgScaleMode, SvgSizing};
