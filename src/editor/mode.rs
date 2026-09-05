use std::fmt;

/// The editor's rendering and interaction mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Read-only preview; no cursor, no raw Markdown. Files open here.
    #[default]
    Preview,

    /// Hybrid editing: the active line (or table cell) shows raw Markdown, the rest renders.
    Rendered,

    /// Whole document as plain Markdown text.
    Raw,

    /// Inline diff review of the pre-change rope vs. on-disk content.
    /// Invariant: `state.mode == Mode::Diff ⟺ state.diff.is_some()`, maintained by
    /// [`super::EditorState::enter_diff_mode`] / [`super::EditorState::exit_diff_mode`].
    Diff,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::Preview => f.write_str("PREVIEW"),
            Mode::Rendered => f.write_str("EDIT"),
            Mode::Raw => f.write_str("RAW"),
            Mode::Diff => f.write_str("DIFF"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_display() {
        assert_eq!(Mode::Preview.to_string(), "PREVIEW");
        assert_eq!(Mode::Rendered.to_string(), "EDIT");
        assert_eq!(Mode::Raw.to_string(), "RAW");
    }

    #[test]
    fn mode_default_is_preview() {
        assert_eq!(Mode::default(), Mode::Preview);
    }
}
