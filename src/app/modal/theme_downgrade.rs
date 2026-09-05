//! Notice that the theme was swapped for an indexed-color built-in
//! ([`crate::config::theme::indexed_fallback_theme`]).  The swap happens *before* the first
//! frame so this modal is legible — a quantized RGB theme can land fg and bg on the same
//! entry, hiding its own explanation — so it reports, rather than asks.  Nothing is written
//! to `config.toml` (the choice likely came from a more capable terminal via dotfiles), and
//! there is deliberately no "switch theme" button: the only other legible theme is the
//! opposite appearance.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::ui::{theme_downgrade_lines, ModalButton, ModalResponse, PROSE_CONTENT_WIDTH};

pub struct ThemeDowngradeModal {
    configured: String,
    substituted: &'static str,
    /// Always empty; keeps the `ModalChrome` calls uniform.
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
}

impl ThemeDowngradeModal {
    pub fn new(configured: String, substituted: &'static str) -> Self {
        Self {
            configured,
            substituted,
            buttons: Vec::new(),
            // Cap the measure: uncapped, one long paragraph stretches the modal terminal-wide.
            chrome: ModalChrome::new(ModalKind::Warning, true)
                .with_max_content_width(PROSE_CONTENT_WIDTH),
        }
    }

    /// Shared by the key and click paths; buttonless, so `Esc` is the only resolution.
    fn resolve(&self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }
}

impl Modal for ThemeDowngradeModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        // Shared with the capabilities notice so the two can't drift.
        let body = theme_downgrade_lines(&self.configured, self.substituted, ctx.theme);
        self.chrome
            .render(frame, area, ctx, "Theme changed", &body, &self.buttons);
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
    use super::*;

    #[test]
    fn the_modal_is_buttonless() {
        let modal = ThemeDowngradeModal::new("Dracula".into(), "256 Dark");
        assert!(modal.buttons.is_empty());
    }
}
