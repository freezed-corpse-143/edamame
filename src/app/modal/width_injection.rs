//! Warning shown when a column-border drag would inject a `<!-- tui-columns: ... -->`
//! comment into a table that has none.  Escape discards the live width preview.  The
//! pending table lives in [`crate::editor::EditorState::pending_column_widths_commit`],
//! not on the modal.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse};

pub struct WidthInjectionWarning {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
}

impl WidthInjectionWarning {
    pub fn new() -> Self {
        Self {
            body: vec![
                Line::raw("Setting custom column widths adds a"),
                Line::raw("<!-- tui-columns: [...] --> comment to the"),
                Line::raw("Markdown source so the layout persists."),
                Line::raw(""),
                Line::raw("Continue?"),
            ],
            buttons: vec![
                ModalButton::new("Continue"),
                ModalButton::new("Continue and don't ask again"),
            ],
            chrome: ModalChrome::new(ModalKind::Warning, true),
        }
    }

    /// Map a chrome response to an outcome; shared by the key and click paths.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::CloseAnd(Box::new(|app| {
                app.editor.cancel_pending_column_widths();
            })),
            ModalResponse::ButtonPressed(0) => ModalOutcome::CloseAnd(Box::new(|app| {
                app.editor.commit_pending_column_widths();
            })),
            ModalResponse::ButtonPressed(_) => ModalOutcome::CloseAnd(Box::new(|app| {
                app.config.table.warn_on_width_injection = false;
                app.save_config_with_flash("failed to persist table.warn_on_width_injection");
                app.editor.commit_pending_column_widths();
            })),
        }
    }
}

impl Default for WidthInjectionWarning {
    fn default() -> Self {
        Self::new()
    }
}

impl Modal for WidthInjectionWarning {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome.render(
            frame,
            area,
            ctx,
            "Custom column widths",
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
