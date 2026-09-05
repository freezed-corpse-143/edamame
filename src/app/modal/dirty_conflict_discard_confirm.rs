//! Destructive confirmation gating [`super::DirtyConflictModal`]'s `[Discard & reload]`.
//! Carries the on-disk contents already read by the watcher so the reload is byte-identical
//! to the triggering change, without a re-read race.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::dirty_conflict::DirtyConflictModal;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse};

pub struct DirtyConflictDiscardConfirmModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
    /// `pub(crate)` for the `file_changed.rs` tests; production code goes through
    /// [`Self::set_on_disk_contents`].
    pub(crate) on_disk_contents: String,
}

impl DirtyConflictDiscardConfirmModal {
    /// Refresh the carried contents when another external write lands while the modal is
    /// open, so the eventual reload uses the current disk state.
    pub fn set_on_disk_contents(&mut self, contents: String) {
        self.on_disk_contents = contents;
    }

    pub fn new(on_disk_contents: String) -> Self {
        let body = vec![
            Line::raw("Discard your unsaved edits?"),
            Line::raw(""),
            Line::raw("They cannot be recovered."),
        ];
        // Cancel takes default focus; the destructive button is deliberately one Tab away.
        Self {
            body,
            buttons: vec![
                ModalButton::new("Cancel"),
                ModalButton::new("Discard & reload"),
            ],
            chrome: ModalChrome::new(ModalKind::Warning, true),
            on_disk_contents,
        }
    }

    /// Map a chrome response to an outcome; shared by the key and click paths.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::Close,
            ModalResponse::ButtonPressed(0) => ModalOutcome::Close,
            ModalResponse::ButtonPressed(1) => {
                let contents = std::mem::take(&mut self.on_disk_contents);
                ModalOutcome::CloseAnd(Box::new(move |app| {
                    app.modal_stack.remove_first::<DirtyConflictModal>();
                    app.reload_buffer_from_disk(contents);
                }))
            }
            ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }
}

impl Modal for DirtyConflictDiscardConfirmModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome.render(
            frame,
            area,
            ctx,
            "Discard unsaved edits?",
            &self.body,
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
