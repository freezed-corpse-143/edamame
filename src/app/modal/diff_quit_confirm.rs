//! Quit confirmation while a diff review is in progress: the review is unapplied work, so
//! this mirrors the dirty-buffer [`super::QuitConfirmModal`].

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse};

/// `Keep reviewing` is index 0 so a bare Enter is the safe choice.
const DISCARD_IDX: usize = 1;

pub struct DiffQuitConfirmModal {
    chrome: ModalChrome,
    buttons: Vec<ModalButton>,
}

impl DiffQuitConfirmModal {
    pub fn new() -> Self {
        Self {
            chrome: ModalChrome::new(ModalKind::Warning, true),
            buttons: vec![
                ModalButton::new("Keep reviewing"),
                ModalButton::new("Discard & quit"),
            ],
        }
    }

    /// Map a chrome response to an outcome; shared by the key and click paths.
    fn resolve(&self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::Close,
            ModalResponse::ButtonPressed(DISCARD_IDX) => ModalOutcome::CloseAnd(Box::new(|app| {
                app.exit_diff_mode_discarding();
                app.should_quit = true;
            })),
            ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }
}

impl Default for DiffQuitConfirmModal {
    fn default() -> Self {
        Self::new()
    }
}

impl Modal for DiffQuitConfirmModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let body = vec![
            Line::raw("You are reviewing changes from disk."),
            Line::raw(""),
            Line::raw("Quitting now discards the review and every decision you've made."),
        ];
        self.chrome.render(
            frame,
            area,
            ctx,
            "Discard diff review?",
            &body,
            &self.buttons,
        );
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        let response = self.chrome.on_key(&key, self.buttons.len());
        self.resolve(response)
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.chrome.on_wheel(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        let response = self.chrome.on_click(col, row);
        self.resolve(response)
    }

    fn kind(&self) -> ModalKind {
        self.chrome.kind()
    }

    fn dismissable(&self) -> bool {
        self.chrome.dismissable()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::test_utils::make_app;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn keep_reviewing_dismisses_without_quitting() {
        let mut app = make_app();
        let mut modal = DiffQuitConfirmModal::new();
        let out = modal.handle_key(key(KeyCode::Enter), &mut app, 40, 80);
        assert!(matches!(out, ModalOutcome::Close));
    }

    #[test]
    fn esc_dismisses_without_quitting() {
        let mut app = make_app();
        let mut modal = DiffQuitConfirmModal::new();
        let out = modal.handle_key(key(KeyCode::Esc), &mut app, 40, 80);
        assert!(matches!(out, ModalOutcome::Close));
    }

    #[test]
    fn discard_and_quit_closes_with_callback() {
        let mut app = make_app();
        let mut modal = DiffQuitConfirmModal::new();
        modal.handle_key(key(KeyCode::Tab), &mut app, 40, 80);
        let out = modal.handle_key(key(KeyCode::Enter), &mut app, 40, 80);
        assert!(matches!(out, ModalOutcome::CloseAnd(_)));
    }
}
