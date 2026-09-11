//! Hunk types for the diff subsystem. Line ranges are half-open `[start, end)`; an
//! `Insert` has an empty `old_lines`, a `Delete` an empty `new_lines`.

use std::ops::Range;

/// Stable per-hunk identifier: monotonically allocated and never reused, even across
/// hunk-list recomputations, so `DiffState::focused_id` and decisions survive index shifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HunkId(pub u64);

/// Per-hunk review decision. Resolution proceeds only when no hunk is `Pending`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Decision {
    #[default]
    Pending,
    Accepted,
    Rejected,
}

/// Derived from the emptiness of `old_lines` / `new_lines` at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkKind {
    Replace,
    Insert,
    Delete,
}

/// A word-level inline highlight within a `Replace` hunk (empty for `Insert` / `Delete`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineSpan {
    pub side: InlineSide,
    /// 0-based line index within the hunk's old- or new-side line range.
    pub line_in_hunk: usize,
    /// Half-open char range within that line's text, excluding the trailing newline.
    pub chars: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineSide {
    Old,
    New,
}

/// A single contiguous diff hunk.
#[derive(Debug, Clone)]
pub struct Hunk {
    pub id: HunkId,
    pub old_lines: Range<usize>,
    pub new_lines: Range<usize>,
    pub inline: Vec<InlineSpan>,
    pub kind: HunkKind,
}

impl Hunk {
    pub(crate) fn classify(old_lines: &Range<usize>, new_lines: &Range<usize>) -> HunkKind {
        let old_empty = old_lines.start == old_lines.end;
        let new_empty = new_lines.start == new_lines.end;
        match (old_empty, new_empty) {
            (true, false) => HunkKind::Insert,
            (false, true) => HunkKind::Delete,
            // (true, true) never comes from the engine; Replace is the harmless default.
            (false, false) | (true, true) => HunkKind::Replace,
        }
    }
}
