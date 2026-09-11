//! Quiet-window debouncer for the watcher worker: a pure state machine over instants. The
//! worker [`Debouncer::record`]s each notify event, uses [`Debouncer::deadline`] as its
//! `recv_timeout`, and reads the file once when [`Debouncer::fire_if_due`] returns true.

use std::time::{Duration, Instant};

pub struct Debouncer {
    window: Duration,
    pending_since: Option<Instant>,
}

impl Debouncer {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            pending_since: None,
        }
    }

    /// Sliding window: an event during a pending interval restarts the timer, so a burst
    /// fires exactly once after it finishes.
    pub fn record(&mut self, now: Instant) {
        self.pending_since = Some(now);
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.pending_since.map(|t| t + self.window)
    }

    /// True while a fire is pending, whether or not the deadline has elapsed.
    #[allow(dead_code)] // used by tests
    pub fn is_pending(&self) -> bool {
        self.pending_since.is_some()
    }

    /// Clears the pending state and returns `true` once `now` reaches the deadline.
    pub fn fire_if_due(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.deadline() else {
            return false;
        };
        if now >= deadline {
            self.pending_since = None;
            true
        } else {
            false
        }
    }

    /// Drop any pending fire without firing (forced reconcile, unwatch).
    pub fn clear(&mut self) {
        self.pending_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Duration {
        Duration::from_millis(200)
    }

    #[test]
    fn fresh_debouncer_is_idle() {
        let d = Debouncer::new(window());
        assert!(!d.is_pending());
        assert!(d.deadline().is_none());
    }

    #[test]
    fn record_arms_the_deadline() {
        let mut d = Debouncer::new(window());
        let t0 = Instant::now();
        d.record(t0);
        assert!(d.is_pending());
        assert_eq!(d.deadline(), Some(t0 + window()));
    }

    #[test]
    fn fire_if_due_returns_false_before_deadline() {
        let mut d = Debouncer::new(window());
        let t0 = Instant::now();
        d.record(t0);
        assert!(!d.fire_if_due(t0 + Duration::from_millis(50)));
        assert!(d.is_pending(), "still pending below deadline");
    }

    #[test]
    fn fire_if_due_clears_state_at_deadline() {
        let mut d = Debouncer::new(window());
        let t0 = Instant::now();
        d.record(t0);
        assert!(d.fire_if_due(t0 + window()));
        assert!(!d.is_pending());
        assert!(d.deadline().is_none());
    }

    #[test]
    fn record_during_pending_extends_window() {
        let mut d = Debouncer::new(window());
        let t0 = Instant::now();
        d.record(t0);
        let t1 = t0 + Duration::from_millis(100);
        d.record(t1);
        assert_eq!(d.deadline(), Some(t1 + window()));
        assert!(!d.fire_if_due(t0 + window()));
    }

    #[test]
    fn clear_discards_pending_state() {
        let mut d = Debouncer::new(window());
        d.record(Instant::now());
        d.clear();
        assert!(!d.is_pending());
        assert!(d.deadline().is_none());
    }

    #[test]
    fn fire_if_due_on_idle_is_a_noop() {
        let mut d = Debouncer::new(window());
        assert!(!d.fire_if_due(Instant::now()));
        assert!(!d.is_pending());
    }
}
