//! Buttonless message modal for warnings, errors, and refusals; the [`ModalKind`] drives
//! the title.  Used instead of a flash where the user must not miss the message.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{ModalButton, ModalResponse};

pub struct NoticeModal {
    title: &'static str,
    text: String,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
}

impl NoticeModal {
    /// Multi-line `text` is split on `\n` at render time.
    pub fn new(text: impl Into<String>, kind: ModalKind) -> Self {
        Self {
            title: title_for(kind),
            text: text.into(),
            buttons: Vec::new(),
            chrome: ModalChrome::new(kind, true),
        }
    }

    /// Raw text, for duplicate detection in [`crate::app::App::notify`].  Deliberately not
    /// on the [`Modal`] trait so no other modal type can be silently deduplicated.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Empty input still yields one blank line so the body is non-zero.
    fn body_lines(&self) -> Vec<Line<'static>> {
        let lines: Vec<Line<'static>> =
            self.text.lines().map(|l| Line::raw(l.to_owned())).collect();
        if lines.is_empty() {
            vec![Line::raw("")]
        } else {
            lines
        }
    }
}

fn title_for(kind: ModalKind) -> &'static str {
    match kind {
        ModalKind::Error => "Error",
        ModalKind::Warning => "Warning",
        ModalKind::Normal => "Notice",
    }
}

impl Modal for NoticeModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let body = self.body_lines();
        self.chrome
            .render(frame, area, ctx, self.title, &body, &self.buttons);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        match self.chrome.on_key(&key, self.buttons.len()) {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.chrome.on_wheel(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        match self.chrome.on_click(col, row) {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
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
    use super::*;
    use crate::app::test_utils::make_app;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn title_follows_kind() {
        assert_eq!(NoticeModal::new("x", ModalKind::Error).title, "Error");
        assert_eq!(NoticeModal::new("x", ModalKind::Warning).title, "Warning");
        assert_eq!(NoticeModal::new("x", ModalKind::Normal).title, "Notice");
    }

    #[test]
    fn multi_line_message_is_split() {
        let m = NoticeModal::new("first\nsecond", ModalKind::Warning);
        let body = m.body_lines();
        assert_eq!(body.len(), 2);
        assert_eq!(body[0].to_string(), "first");
        assert_eq!(body[1].to_string(), "second");
    }

    #[test]
    fn empty_message_still_renders_one_line() {
        let m = NoticeModal::new("", ModalKind::Warning);
        assert_eq!(m.body_lines().len(), 1);
    }

    #[test]
    fn escape_dismisses() {
        let mut app = make_app();
        app.modal_stack
            .push(Box::new(NoticeModal::new("boom", ModalKind::Error)));
        assert!(app.modal_stack.contains::<NoticeModal>());
        app.dispatch_modal_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 40, 80);
        assert!(!app.modal_stack.contains::<NoticeModal>());
    }
}
