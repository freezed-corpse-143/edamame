//! Figures-enabled prompt (Yes / No / Always / Never; Esc = No), shown when
//! `config.figures.enabled` is `Ask` and the document has a figure — a diagram code block (e.g.
//! ```mermaid) or a `$$...$$` display-math paragraph.  Yes / No affect only this session; Always /
//! Never persist.  Deliberately independent of [`super::ImagesEnabledPromptModal`] so the two
//! opt-ins are separate.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::config::Config;
use crate::editor::EditorState;
use crate::ui::{ModalButton, ModalResponse};

pub struct FiguresEnabledPromptModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
}

impl FiguresEnabledPromptModal {
    /// `None` unless the policy is `Ask` and the document has a figure (a diagram or `$$...$$`
    /// math block).
    pub fn from_state(editor: &EditorState, config: &Config) -> Option<Self> {
        if !matches!(config.figures.enabled, crate::config::FiguresEnabled::Ask) {
            return None;
        }
        let has_diagram = editor
            .parsed
            .image_blocks
            .iter()
            .any(|b| b.source.is_some());
        if !has_diagram {
            return None;
        }
        let body = vec![
            Line::raw("This document contains figures (diagrams or math)."),
            Line::raw(""),
            Line::raw("Would you like edamame to display them?"),
        ];
        Some(Self {
            body,
            buttons: vec![
                ModalButton::new("Yes"),
                ModalButton::new("No"),
                ModalButton::new("Always"),
                ModalButton::new("Never"),
            ],
            chrome: ModalChrome::new(ModalKind::Warning, true),
        })
    }

    /// Map a chrome response to an outcome; shared by the key and click paths.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::CloseAnd(Box::new(decline_for_session)),
            ModalResponse::ButtonPressed(idx) => match idx {
                0 => ModalOutcome::CloseAnd(Box::new(|app| {
                    app.session_diagrams_enabled = Some(true);
                    app.dispatch_image_decodes();
                })),
                1 => ModalOutcome::CloseAnd(Box::new(decline_for_session)),
                2 => ModalOutcome::CloseAnd(Box::new(|app| {
                    app.config.figures.enabled = crate::config::FiguresEnabled::Always;
                    app.save_config_with_flash("failed to persist figures.enabled=always");
                    app.dispatch_image_decodes();
                })),
                _ => ModalOutcome::CloseAnd(Box::new(|app| {
                    app.config.figures.enabled = crate::config::FiguresEnabled::Never;
                    app.save_config_with_flash("failed to persist figures.enabled=never");
                    decline_for_session(app);
                })),
            },
        }
    }
}

impl Modal for FiguresEnabledPromptModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome
            .render(frame, area, ctx, "Figures", &self.body, &self.buttons);
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

/// Session opt-out: collapse diagram blocks to placeholders and re-parse.
fn decline_for_session(app: &mut App) {
    app.session_diagrams_enabled = Some(false);
    app.editor.diagrams_enabled = false;
    app.editor.refresh_parsed();
}
