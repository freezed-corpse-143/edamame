//! Deferred post-decision focus advance for diff review: after accept/reject the resolved
//! hunk stays focused for [`DIFF_ADVANCE_DELAY`] so the user sees the decision land before
//! focus jumps to the next pending hunk.  Same timer shape as [`super::section_jump`];
//! rapid tapping flushes the pending advance first ([`App::apply_diff_advance`]).

use std::time::{Duration, Instant};

use super::App;

/// How long a resolved hunk stays focused before focus auto-advances.
pub(super) const DIFF_ADVANCE_DELAY: Duration = Duration::from_millis(350);

impl App {
    /// Arm (or re-arm) the post-decision advance timer.
    pub(crate) fn arm_diff_advance(&mut self) {
        self.diff_advance_pending_since = Some(Instant::now());
    }

    /// Drop a pending advance when the user takes manual control (navigation, exit,
    /// accept-all / reject-all).
    pub(crate) fn cancel_diff_advance(&mut self) {
        self.diff_advance_pending_since = None;
    }

    /// Perform the deferred advance now and re-check resolution (which may pop the confirm
    /// modal).  Clears the timer unconditionally so it doubles as a flush before a fresh
    /// decision.
    pub(crate) fn apply_diff_advance(&mut self) {
        self.diff_advance_pending_since = None;
        if let Some(d) = self.editor.diff.as_mut() {
            d.advance_to_next_pending();
            // Viewport height is unknown here; the next `prepare_viewport` scrolls.
            self.editor.pending_focus_scroll = true;
            self.needs_draw = true;
        }
        self.check_diff_resolution();
    }

    /// Per-iteration step: advance once the reveal window has elapsed.
    pub(super) fn tick_diff_advance(&mut self) {
        let Some(since) = self.diff_advance_pending_since else {
            return;
        };
        if since.elapsed() < DIFF_ADVANCE_DELAY {
            return;
        }
        self.apply_diff_advance();
    }

    /// When the run loop must wake to fire a pending advance (feeds [`App::next_deadline`]).
    pub(super) fn diff_advance_deadline(&self) -> Option<Instant> {
        self.diff_advance_pending_since
            .map(|t| t + DIFF_ADVANCE_DELAY)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::app::test_utils::make_app;
    use crate::diff::{Decision, DiffState};

    fn app_in_diff() -> App {
        let mut app = make_app();
        let old = "a\nb\nc\nd\ne\n";
        let new = "A\nb\nC\nd\nE\n"; // hunks at lines 0, 2, 4
        let diff = DiffState::new(old, new).expect("non-empty diff");
        assert_eq!(diff.hunks.len(), 3);
        app.editor.enter_diff_mode(diff);
        app
    }

    #[test]
    fn decision_defers_advance_until_window_elapses() {
        let mut app = app_in_diff();
        let first = app.editor.diff.as_ref().unwrap().focused_id;

        app.editor
            .diff
            .as_mut()
            .unwrap()
            .decide_focused(Decision::Accepted);
        app.arm_diff_advance();

        assert_eq!(app.editor.diff.as_ref().unwrap().focused_id, first);
        assert!(app.diff_advance_pending_since.is_some());

        app.tick_diff_advance();
        assert_eq!(app.editor.diff.as_ref().unwrap().focused_id, first);

        app.diff_advance_pending_since =
            Some(Instant::now() - DIFF_ADVANCE_DELAY - Duration::from_millis(5));
        app.tick_diff_advance();
        assert_ne!(app.editor.diff.as_ref().unwrap().focused_id, first);
        assert!(app.diff_advance_pending_since.is_none());
        assert!(app.editor.pending_focus_scroll);
    }

    #[test]
    fn cancel_drops_pending_advance() {
        let mut app = app_in_diff();
        let first = app.editor.diff.as_ref().unwrap().focused_id;

        app.editor
            .diff
            .as_mut()
            .unwrap()
            .decide_focused(Decision::Accepted);
        app.arm_diff_advance();
        app.diff_advance_pending_since =
            Some(Instant::now() - DIFF_ADVANCE_DELAY - Duration::from_millis(5));

        app.cancel_diff_advance();
        assert!(app.diff_advance_pending_since.is_none());
        app.tick_diff_advance();
        assert_eq!(app.editor.diff.as_ref().unwrap().focused_id, first);
    }

    #[test]
    fn resolving_last_hunk_pops_confirm_modal_after_delay() {
        let mut app = app_in_diff();
        for _ in 0..3 {
            app.editor
                .diff
                .as_mut()
                .unwrap()
                .decide_focused(Decision::Accepted);
            app.diff_advance_pending_since =
                Some(Instant::now() - DIFF_ADVANCE_DELAY - Duration::from_millis(5));
            app.tick_diff_advance();
        }
        assert!(app
            .modal_stack
            .contains::<crate::app::modal::DiffResolveConfirmModal>());
    }
}
