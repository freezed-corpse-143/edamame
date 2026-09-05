//! Crate-wide constants shared across modules.

/// Spaces per indentation level, everywhere indentation is produced or consumed.  Fixed at 4 to
/// follow CommonMark (a nested block must clear a single-digit ordered marker) and to keep the
/// rendered and raw views in lockstep, so de-rendering a block causes no horizontal jump.
pub const INDENT_WIDTH: usize = 4;
