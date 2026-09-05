//! Post-upgrade notice: edamame was updated, here is what changed.
//!
//! Two entry points, one body.  [`PostUpgradeModal::for_upgrade`] is the startup notice and returns
//! `None` when the installed version has no changelog section, so the startup path stays silent
//! rather than raising an empty modal.  [`PostUpgradeModal::on_demand`] is the About page's button
//! and always builds — an explicit request is answered even when the answer is "there aren't any",
//! the same split [`super::update`] draws.
//!
//! The content is a compiled-in string, so nothing animates and nothing arrives later: no
//! `next_deadline`, no `set_status`, no buttons.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::post_upgrade::changelog;
use crate::app::update_check::INSTALLED_VERSION;
use crate::app::App;
use crate::ui::update_check::{self as ui_update, PostUpgradeOccasion, PostUpgradeReport};
use crate::ui::{ModalResponse, PROSE_CONTENT_WIDTH};

pub struct PostUpgradeModal {
    chrome: ModalChrome,
    /// `None` means no changelog section for this version, distinct from `Some(vec![])`, a section
    /// that exists and says nothing.  Only the first is reported as an absence.
    notes: Option<Vec<String>>,
    /// Which entry point built this.  It decides only the body's opening line, but must live on the
    /// modal because `render` is where the two paths meet again.
    occasion: PostUpgradeOccasion,
}

impl PostUpgradeModal {
    /// The startup notice; `None` when there is nothing to show, so the caller needs no test of
    /// its own.
    pub(crate) fn for_upgrade() -> Option<Self> {
        Some(Self::new(
            Some(changelog::notes_for_version(INSTALLED_VERSION)?),
            PostUpgradeOccasion::Upgrade,
        ))
    }

    /// The About page's opening, which always has something to say.
    pub(crate) fn on_demand() -> Self {
        Self::new(
            changelog::notes_for_version(INSTALLED_VERSION),
            PostUpgradeOccasion::OnDemand,
        )
    }

    fn new(notes: Option<Vec<String>>, occasion: PostUpgradeOccasion) -> Self {
        Self {
            // Prose, so the width is capped: longest-line sizing would stretch it across the
            // terminal.  Same as `UpdateModal`.
            chrome: ModalChrome::new(ModalKind::Normal, true)
                .with_max_content_width(PROSE_CONTENT_WIDTH),
            notes,
            occasion,
        }
    }

    /// Translate into the `ui` layer's vocabulary, keeping `ui::update_check` free of `app`.
    fn report(&self) -> PostUpgradeReport<'_> {
        match &self.notes {
            Some(notes) => PostUpgradeReport::Found { notes },
            None => PostUpgradeReport::NotFound,
        }
    }

    /// Shared by the key and click paths.  With no buttons, everything but a scroll closes.
    fn resolve(response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => ModalOutcome::Close,
        }
    }
}

impl Modal for PostUpgradeModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let body = ui_update::post_upgrade_body_lines(
            ctx.theme,
            self.occasion,
            self.report(),
            INSTALLED_VERSION,
        );
        self.chrome
            .render(frame, area, ctx, "Release notes", &body, &[]);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        Self::resolve(self.chrome.on_key(&key, 0))
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.chrome.on_wheel(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        Self::resolve(self.chrome.on_click(col, row))
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

    fn with_notes() -> PostUpgradeModal {
        PostUpgradeModal::new(
            Some(vec!["- a thing".to_owned()]),
            PostUpgradeOccasion::Upgrade,
        )
    }

    #[test]
    fn esc_dismisses() {
        let mut app = make_app();
        let mut modal = with_notes();
        assert!(matches!(
            modal.handle_key(key(KeyCode::Esc), &mut app, 24, 80),
            ModalOutcome::Close
        ));
    }

    #[test]
    fn enter_does_nothing_since_there_is_nothing_to_press() {
        // `NoticeModal`'s button-less shape: no footer row, so Esc is the way out.
        let mut app = make_app();
        let mut modal = with_notes();
        assert!(matches!(
            modal.handle_key(key(KeyCode::Enter), &mut app, 24, 80),
            ModalOutcome::Continue
        ));
    }

    #[test]
    fn a_missing_section_and_an_empty_one_are_different_states() {
        assert!(matches!(
            PostUpgradeModal::new(None, PostUpgradeOccasion::OnDemand).report(),
            PostUpgradeReport::NotFound
        ));
        assert!(matches!(
            PostUpgradeModal::new(Some(Vec::new()), PostUpgradeOccasion::OnDemand).report(),
            PostUpgradeReport::Found { .. }
        ));
    }

    #[test]
    fn the_on_demand_opening_always_builds_a_modal() {
        // Including when the bundled changelog says nothing, the normal state between releases.
        let modal = PostUpgradeModal::on_demand();
        assert!(modal.dismissable());
        assert_eq!(modal.kind(), ModalKind::Normal);
    }

    #[test]
    fn each_entry_point_carries_its_own_occasion() {
        // The About page's opening must not announce an upgrade; the startup notice must.
        assert_eq!(
            PostUpgradeModal::on_demand().occasion,
            PostUpgradeOccasion::OnDemand
        );
        if let Some(modal) = PostUpgradeModal::for_upgrade() {
            assert_eq!(modal.occasion, PostUpgradeOccasion::Upgrade);
        }
    }
}
