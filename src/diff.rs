//! Diff-mode subsystem: [`engine`] (pure line + word diff), [`hunk`] (data types),
//! [`state::DiffState`] (session state owned by `EditorState::diff` while `Mode::Diff` is
//! active), and [`layout`] (the stacked visual-line model shared by renderer and scroll math).
//! See `docs/dev/diff-review.md`.

pub mod engine;
pub mod hunk;
pub mod layout;
pub mod state;

#[allow(unused_imports)]
pub use engine::{compute, HunkIdAllocator};
#[allow(unused_imports)]
pub use hunk::{Decision, Hunk, HunkId, HunkKind, InlineSide, InlineSpan};
#[allow(unused_imports)]
pub use layout::{DiffLineSource, DiffVisualLine};
pub use state::{DiffState, ReconcileOutcome};
