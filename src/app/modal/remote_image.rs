//! Remote-image fetch prompt.  Shown when `config.images.remote_policy` is `Ask` and the
//! open document references at least one `http(s)://` image.  Yes / No affect only the
//! current session; Always / Never persist to config.  Escape is equivalent to "No".

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

pub struct RemoteImagePromptModal {
    body: Vec<Line<'static>>,
    buttons: Vec<ModalButton>,
    chrome: ModalChrome,
}

impl RemoteImagePromptModal {
    /// `None` unless the document references an `http(s)://` image and the policy is `Ask`.
    pub fn from_state(editor: &EditorState, config: &Config) -> Option<Self> {
        if matches!(config.images.enabled, crate::config::ImagesEnabled::Never) {
            return None;
        }
        if config.images.remote_policy != crate::config::RemoteImagePolicy::Ask {
            return None;
        }
        let has_remote = editor
            .parsed
            .image_blocks
            .iter()
            .any(|b| crate::image::loader::is_remote(&b.url));
        if !has_remote {
            return None;
        }
        let body = vec![
            Line::raw("This document references one or more remote images."),
            Line::raw("Fetching them sends HTTP requests from your machine."),
            Line::raw(""),
            Line::raw("Would you like edamame to fetch remote images?"),
        ];
        // "Yes" is leftmost, hence the default focus: session-only, so it is the safe
        // answer if the user hammers Enter without reading.
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

    /// Shared by the key and click paths so a click behaves exactly like the key.
    fn resolve(&mut self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled => ModalOutcome::CloseAnd(Box::new(decline_for_session)),
            ModalResponse::ButtonPressed(idx) => {
                // 0 Yes, 1 No (both session-only), 2 Always, 3 Never (both persisted).
                match idx {
                    0 => ModalOutcome::CloseAnd(Box::new(|app| {
                        app.session_allow_remote = true;
                        // The two session flags can never both be set.
                        app.session_remote_declined = false;
                        app.editor.images.clear_failures_for_remote_reopening();
                        app.dispatch_image_decodes();
                    })),
                    1 => ModalOutcome::CloseAnd(Box::new(decline_for_session)),
                    2 => ModalOutcome::CloseAnd(Box::new(|app| {
                        app.config.images.remote_policy = crate::config::RemoteImagePolicy::Always;
                        app.session_remote_declined = false;
                        app.save_config_with_flash("failed to persist remote_policy=Always");
                        app.editor.images.clear_failures_for_remote_reopening();
                        app.dispatch_image_decodes();
                    })),
                    _ => ModalOutcome::CloseAnd(Box::new(|app| {
                        app.config.images.remote_policy = crate::config::RemoteImagePolicy::Never;
                        // Inert on its own (the persisted `Never` stops later prompts),
                        // but keeps the "never both set" invariant true at all four arms.
                        app.session_remote_declined = false;
                        app.save_config_with_flash("failed to persist remote_policy=Never");
                    })),
                }
            }
        }
    }
}

impl Modal for RemoteImagePromptModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        self.chrome
            .render(frame, area, ctx, "Remote Images", &self.body, &self.buttons);
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

/// Decline remote fetches for the rest of the session without touching
/// `config.images.remote_policy`.  Recorded rather than just closing the modal so a
/// document opened later in the session doesn't re-ask.
fn decline_for_session(app: &mut App) {
    app.session_remote_declined = true;
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::modal::types::ModalOutcome;
    use crate::app::test_utils::app_with_buffer;

    /// Drive the modal with `key` and run the outcome, exactly as the modal stack would.
    fn press(app: &mut App, key: KeyCode) {
        let mut modal = RemoteImagePromptModal::from_state(&app.editor, &app.config)
            .expect("a remote image and the `Ask` policy warrant the prompt");
        let outcome = modal.handle_key(KeyEvent::new(key, KeyModifiers::NONE), app, 20, 80);
        match outcome {
            ModalOutcome::CloseAnd(f) => f(app),
            _ => panic!("expected CloseAnd"),
        }
    }

    fn app() -> App {
        app_with_buffer("![a](https://example.com/a.png)\n", 0)
    }

    #[test]
    fn escape_declines_for_the_session() {
        let mut a = app();
        press(&mut a, KeyCode::Esc);
        assert!(!a.session_allow_remote);
        assert!(
            a.session_remote_declined,
            "a dismissed prompt must not re-ask on the next document"
        );
        assert_eq!(
            a.config.images.remote_policy,
            crate::config::RemoteImagePolicy::Ask,
            "a session decline persists nothing"
        );
    }

    #[test]
    fn the_no_button_declines_for_the_session() {
        let mut a = app();
        let mut modal = RemoteImagePromptModal::from_state(&a.editor, &a.config).expect("prompt");
        let _ = modal.handle_key(
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
            &mut a,
            20,
            80,
        );
        let outcome = modal.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut a,
            20,
            80,
        );
        match outcome {
            ModalOutcome::CloseAnd(f) => f(&mut a),
            _ => panic!("expected CloseAnd"),
        }
        assert!(!a.session_allow_remote);
        assert!(a.session_remote_declined);
    }

    #[test]
    fn yes_allows_remote_and_clears_any_decline() {
        let mut a = app();
        a.session_remote_declined = true;
        press(&mut a, KeyCode::Enter); // focus starts on `Yes`
        assert!(a.session_allow_remote);
        assert!(
            !a.session_remote_declined,
            "the two session flags must never both be set"
        );
    }

    #[test]
    fn always_clears_a_session_decline() {
        // The answer persists, so isolate the developer's own `config.toml`.
        let _iso = crate::test_env::config_isolation();
        let mut a = app();
        a.session_remote_declined = true;
        let mut modal = RemoteImagePromptModal::from_state(&a.editor, &a.config).expect("prompt");
        for _ in 0..2 {
            let _ = modal.handle_key(
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
                &mut a,
                20,
                80,
            );
        }
        let outcome = modal.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut a,
            20,
            80,
        );
        match outcome {
            ModalOutcome::CloseAnd(f) => f(&mut a),
            _ => panic!("expected CloseAnd"),
        }
        assert_eq!(
            a.config.images.remote_policy,
            crate::config::RemoteImagePolicy::Always
        );
        assert!(!a.session_remote_declined);
    }

    #[test]
    fn never_clears_a_session_decline() {
        // Pinned so all four arms agree that a fresh answer clears the session decline.
        // The answer persists, so isolate the developer's own `config.toml`.
        let _iso = crate::test_env::config_isolation();
        let mut a = app();
        a.session_remote_declined = true;
        let mut modal = RemoteImagePromptModal::from_state(&a.editor, &a.config).expect("prompt");
        for _ in 0..3 {
            let _ = modal.handle_key(
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
                &mut a,
                20,
                80,
            );
        }
        let outcome = modal.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut a,
            20,
            80,
        );
        match outcome {
            ModalOutcome::CloseAnd(f) => f(&mut a),
            _ => panic!("expected CloseAnd"),
        }
        assert_eq!(
            a.config.images.remote_policy,
            crate::config::RemoteImagePolicy::Never
        );
        assert!(!a.session_remote_declined);
    }

    #[test]
    fn a_persisted_policy_change_supersedes_a_session_decline() {
        let mut a = app();
        a.session_remote_declined = true;
        a.config.images.remote_policy = crate::config::RemoteImagePolicy::Ask;
        a.apply_remote_policy_change();
        assert!(
            !a.session_remote_declined,
            "changing the setting re-opens the question"
        );
        assert!(a.modal_stack.contains::<RemoteImagePromptModal>());
    }
}
