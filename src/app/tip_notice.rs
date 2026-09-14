//! App-level orchestration for the daily tip: when to show one, and how it defers to the update
//! check so the two never pop together.  [`super::tips`] is the pure registry; this is the plumbing
//! around it.
//!
//! Unlike the update check ([`super::update_notice`]) there is no network half — a tip is chosen
//! locally and shown synchronously.  The only subtlety is timing: to honor "prefer an update, don't
//! show a tip" the tip has to know whether the *asynchronous* update check will raise a notice,
//! which lands a beat after startup.  So [`App::tick_daily_tip`] waits out an in-flight check, but
//! only within a short grace window ([`TIP_STARTUP_GRACE`]) — past it the user is assumed to be
//! mid-task, and a tip that can't appear promptly is skipped until next launch rather than allowed
//! to interrupt.

use std::time::{Duration, Instant};

use super::update_check::now_unix;
use super::{modal, tips, App};

/// How long after the first frame a daily tip may still appear.  Past this the tip yields until the
/// next launch: it bounds how long the tip waits on a slow update check, and keeps a tip off a
/// startup that is still showing the welcome or capabilities modal.
const TIP_STARTUP_GRACE: Duration = Duration::from_secs(2);

impl App {
    /// Show today's tip once startup has settled — after the update check has resolved, only when
    /// nothing else is on screen, and only inside the grace window.
    ///
    /// A `tick_timers` member rather than a one-shot call because it may have to wait for the async
    /// update result before it can tell whether an update notice will pre-empt the tip.
    pub(super) fn tick_daily_tip(&mut self) {
        if !self.startup_tip_due {
            return;
        }
        // Anchored to the first frame this runs, not to `App::new`: the window measures interactive
        // time, and the run loop has not started at construction.
        let deadline = *self
            .tip_deadline
            .get_or_insert_with(|| Instant::now() + TIP_STARTUP_GRACE);
        if Instant::now() >= deadline {
            // Missed the window — a slow update check, or a startup modal that lingered.  Skip
            // without stamping, so the tip is simply due again next launch rather than lost.
            self.startup_tip_due = false;
            return;
        }
        // Prefer an update: while its check is still running we cannot yet know whether it will
        // notify, so wait it out (bounded by the deadline above).
        if self.release_check_in_flight {
            return;
        }
        // The check has settled.  If it armed a notice — parked, or already on screen — the update
        // wins and the tip stays silent and un-stamped, so it retries tomorrow.
        if self.pending_update_notice.is_some() || self.modal_stack.contains::<modal::UpdateModal>()
        {
            self.startup_tip_due = false;
            return;
        }
        // Some other modal (welcome, capabilities, a config warning) still owns the screen.  Wait;
        // the deadline gives up if it lingers, which is what keeps a tip off the first-run flow.
        if !self.modal_stack.is_empty() {
            return;
        }
        // Clear to show.  With every tip already seen there is nothing to show and nothing to
        // stamp — the gate will simply re-evaluate next launch.
        let Some(tip) = tips::next_unseen(&self.state.seen_daily_tips) else {
            self.startup_tip_due = false;
            return;
        };
        self.mark_tip_seen(tip.id);
        self.modal_stack
            .push(Box::new(modal::DailyTipModal::new(tip)));
        self.startup_tip_due = false;
        self.needs_draw = true;
    }

    /// Open the "Browse tips" index (the `BrowseTips` palette action).  Idempotent, so the
    /// action can't stack two copies.
    pub(crate) fn open_tips_index(&mut self) {
        if self.modal_stack.contains::<modal::TipsIndexModal>() {
            return;
        }
        self.modal_stack
            .push(Box::new(modal::TipsIndexModal::new()));
        self.needs_draw = true;
    }

    /// Show one tip's modal on demand, from the index.  Uses the `browsing` variant — no "Don't
    /// show tips" off switch, since the reader sought this tip out.  Deliberately does *not* mark
    /// it seen: browsing is independent of the daily rotation, so reading a tip here never consumes
    /// it.
    pub(crate) fn open_tip(&mut self, tip: &'static tips::Tip) {
        self.modal_stack
            .push(Box::new(modal::DailyTipModal::browsing(tip)));
        self.needs_draw = true;
    }

    /// Record that today's tip fired: add its id to the seen set and stamp the clock, so it never
    /// repeats and the next tip waits a full interval.  Background bookkeeping, so no flash.
    fn mark_tip_seen(&mut self, id: u32) {
        if !self.state.seen_daily_tips.contains(&id) {
            self.state.seen_daily_tips.push(id);
        }
        self.state.last_tip_shown = now_unix();
        self.save_state_bookkeeping("daily-tip bookkeeping");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_utils::make_app;

    /// A `make_app()` plus config isolation: `mark_tip_seen` reaches `State::save`, and the
    /// disable button reaches `Config::save`; unguarded, both would rewrite the developer's own
    /// files.  Returned as a tuple so the guard outlives the test body.
    fn isolated_app() -> (crate::test_env::ConfigIsolation, App) {
        let iso = crate::test_env::config_isolation();
        let app = make_app();
        (iso, app)
    }

    /// Drain whatever `App::new` queued (a default config always shows the welcome) so the tip can
    /// reach the empty-stack state it waits for.
    fn clear_modals(app: &mut App) {
        while !app.modal_stack.is_empty() {
            app.modal_stack.pop();
        }
    }

    #[test]
    fn a_due_tip_shows_once_the_stack_is_clear() {
        let (_iso, mut app) = isolated_app();
        app.startup_tip_due = true;
        clear_modals(&mut app);

        app.tick_daily_tip();
        assert!(app.modal_stack.contains::<modal::DailyTipModal>());
        assert!(!app.startup_tip_due, "consumed");
        // Shown once: the id is recorded and the clock stamped.
        assert_eq!(app.state.seen_daily_tips, vec![1]);
        assert_ne!(app.state.last_tip_shown, 0);
    }

    #[test]
    fn a_tip_waits_behind_a_startup_modal_then_gives_up_past_the_window() {
        let (_iso, mut app) = isolated_app();
        app.startup_tip_due = true;
        // The welcome (or any modal) holds the stack.
        assert!(!app.modal_stack.is_empty());

        app.tick_daily_tip();
        assert!(app.startup_tip_due, "still waiting behind the modal");
        assert!(!app.modal_stack.contains::<modal::DailyTipModal>());

        // Force the window shut; the next tick skips this launch without stamping.
        app.tip_deadline = Some(Instant::now() - Duration::from_secs(1));
        app.tick_daily_tip();
        assert!(!app.startup_tip_due, "gave up this launch");
        assert!(app.state.seen_daily_tips.is_empty(), "nothing stamped");
        assert_eq!(app.state.last_tip_shown, 0);
    }

    #[test]
    fn an_in_flight_update_check_holds_the_tip() {
        let (_iso, mut app) = isolated_app();
        app.startup_tip_due = true;
        clear_modals(&mut app);
        app.release_check_in_flight = true;

        app.tick_daily_tip();
        assert!(app.startup_tip_due, "deferred to the pending update result");
        assert!(!app.modal_stack.contains::<modal::DailyTipModal>());
    }

    #[test]
    fn a_pending_update_notice_suppresses_the_tip() {
        let (_iso, mut app) = isolated_app();
        app.startup_tip_due = true;
        clear_modals(&mut app);
        app.pending_update_notice = Some(super::super::update_check::ReleaseInfo {
            tag: "v999.0.0".to_owned(),
            notes: Vec::new(),
        });

        app.tick_daily_tip();
        assert!(!app.startup_tip_due, "yielded to the update");
        assert!(!app.modal_stack.contains::<modal::DailyTipModal>());
        assert!(
            app.state.seen_daily_tips.is_empty(),
            "un-stamped, retries tomorrow"
        );
    }

    #[test]
    fn nothing_shows_when_every_tip_is_seen() {
        let (_iso, mut app) = isolated_app();
        app.state.seen_daily_tips = tips::ALL_TIPS.iter().map(|t| t.id).collect();
        app.startup_tip_due = true;
        clear_modals(&mut app);

        app.tick_daily_tip();
        assert!(!app.modal_stack.contains::<modal::DailyTipModal>());
        assert!(!app.startup_tip_due, "consumed with nothing to show");
        assert_eq!(
            app.state.last_tip_shown, 0,
            "an un-shown tip stamps no clock"
        );
    }
}
