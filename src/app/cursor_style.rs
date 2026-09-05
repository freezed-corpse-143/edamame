//! The single place the (view mode, vim sub-mode) → block-cursor style decision is made.
//!
//! The cursor always mirrors the status chip: every branch reads a `status_mode_*` field
//! (minus `BOLD`), so chip and cursor can never drift.  RAW is signalled only in INSERT;
//! NORMAL / VISUAL keep their sub-mode color in every view, matching the chip.

use ratatui::style::{Modifier, Style};

use crate::config::Theme;
use crate::editor::Mode;
use crate::input::VimSubMode;

/// The block-cursor style for the editor, given the current view `mode` and
/// the active vim sub-mode (`None` for the default handler).
pub fn editor_cursor_style(theme: &Theme, mode: Mode, vim: Option<VimSubMode>) -> Style {
    if mode == Mode::Preview {
        return unbold(theme.status_mode_preview);
    }
    match vim {
        None => {
            if mode == Mode::Raw {
                unbold(theme.status_mode_raw)
            } else {
                unbold(theme.status_mode_rendered)
            }
        }
        Some(VimSubMode::Normal | VimSubMode::OperatorPending) => {
            unbold(theme.status_mode_vim_normal)
        }
        Some(VimSubMode::Insert) => {
            if mode == Mode::Raw {
                unbold(theme.status_mode_raw)
            } else {
                unbold(theme.status_mode_vim_insert)
            }
        }
        Some(VimSubMode::Visual | VimSubMode::VisualLine) => unbold(theme.status_mode_vim_visual),
    }
}

fn unbold(style: Style) -> Style {
    style.remove_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::default()
    }

    #[test]
    fn default_handler_follows_view_mode_chip() {
        let t = theme();
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, None).bg,
            t.status_mode_rendered.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Raw, None).bg,
            t.status_mode_raw.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Preview, None).bg,
            t.status_mode_preview.bg
        );
    }

    #[test]
    fn vim_modes_mirror_their_chip_color() {
        let t = theme();
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, Some(VimSubMode::Normal)).bg,
            t.status_mode_vim_normal.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, Some(VimSubMode::Insert)).bg,
            t.status_mode_vim_insert.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, Some(VimSubMode::Visual)).bg,
            t.status_mode_vim_visual.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, Some(VimSubMode::VisualLine)).bg,
            t.status_mode_vim_visual.bg
        );
    }

    #[test]
    fn operator_pending_reads_as_normal() {
        let t = theme();
        assert_eq!(
            editor_cursor_style(&t, Mode::Rendered, Some(VimSubMode::OperatorPending)).bg,
            t.status_mode_vim_normal.bg
        );
    }

    #[test]
    fn raw_view_only_overrides_insert() {
        let t = theme();
        assert_eq!(
            editor_cursor_style(&t, Mode::Raw, Some(VimSubMode::Insert)).bg,
            t.status_mode_raw.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Raw, Some(VimSubMode::Normal)).bg,
            t.status_mode_vim_normal.bg
        );
        assert_eq!(
            editor_cursor_style(&t, Mode::Raw, Some(VimSubMode::Visual)).bg,
            t.status_mode_vim_visual.bg
        );
    }

    #[test]
    fn cursor_drops_the_chip_bold() {
        let t = theme();
        let style = editor_cursor_style(&t, Mode::Rendered, Some(VimSubMode::Insert));
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }
}
