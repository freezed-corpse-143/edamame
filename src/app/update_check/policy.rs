//! When to check, and when a result deserves to interrupt.
//!
//! Both are pure functions of primitives rather than of `Config` or `App`, which makes the nag
//! behavior table-testable and keeps the two rules a user actually feels — checked at most daily,
//! told at most once per version — in one place instead of split across spawn site and handler.

use super::status::{ReleaseInfo, ReleaseStatus};

/// Minimum gap between automatic checks.  Manual checks ignore it: it bounds *unattended* network
/// chatter, and an explicit request is not unattended.
pub(crate) const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Seconds since the Unix epoch, or `0` if the clock predates it — which reads as "never checked"
/// and so fails safe.
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Has a full [`CHECK_INTERVAL_SECS`] passed since `last`?  The pure once-a-day gate shared by
/// the update check and the daily tip, each over its own timestamp.
///
/// `last == 0` (never) is spelled out rather than left to the arithmetic, which would only agree on
/// a machine whose clock has already passed the interval since the epoch.  A `last` in the future
/// means the clock moved backwards, and is treated as elapsed rather than letting a bogus timestamp
/// disable the gate for what could be years.
pub(crate) fn interval_elapsed(last: u64, now: u64) -> bool {
    last == 0 || last > now || now - last >= CHECK_INTERVAL_SECS
}

/// Should the startup check hit the network?  The daily [`interval_elapsed`] gate, plus the config
/// flag — kept a plain bool so this stays a pure function of primitives.
pub(crate) fn network_check_due(enabled: bool, last_check: u64, now: u64) -> bool {
    enabled && interval_elapsed(last_check, now)
}

/// Should this result raise the startup notice?  Only for a newer release the user hasn't been
/// told about: `UpToDate` is the silence the feature exists for, `Inconclusive` is not grounds to
/// interrupt anyone, and a `Failed` unattended check is not the user's problem to dismiss.
pub(crate) fn notice_due<'a>(
    status: &'a ReleaseStatus,
    notified_for: &str,
) -> Option<&'a ReleaseInfo> {
    match status {
        ReleaseStatus::Available(info) if info.tag != notified_for => Some(info),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = CHECK_INTERVAL_SECS;

    fn available(tag: &str) -> ReleaseStatus {
        ReleaseStatus::Available(ReleaseInfo {
            tag: tag.to_owned(),
            notes: Vec::new(),
        })
    }

    #[test]
    fn a_disabled_check_is_never_due() {
        assert!(!network_check_due(false, 0, DAY * 100));
        assert!(!network_check_due(false, 0, 0));
    }

    #[test]
    fn a_never_run_check_is_due_immediately() {
        assert!(network_check_due(true, 0, 1));
    }

    #[test]
    fn a_check_inside_the_window_is_not_due() {
        let now = DAY * 10;
        assert!(!network_check_due(true, now, now));
        assert!(!network_check_due(true, now - 1, now));
        assert!(!network_check_due(true, now - (DAY - 1), now));
    }

    #[test]
    fn a_check_at_or_past_the_window_is_due() {
        let now = DAY * 10;
        assert!(network_check_due(true, now - DAY, now));
        assert!(network_check_due(true, now - DAY * 3, now));
    }

    #[test]
    fn a_timestamp_from_the_future_is_due_rather_than_stuck() {
        assert!(network_check_due(true, DAY * 100, DAY * 10));
    }

    #[test]
    fn the_shared_gate_treats_never_and_a_future_stamp_as_elapsed() {
        assert!(interval_elapsed(0, DAY * 10), "never");
        assert!(interval_elapsed(DAY * 100, DAY * 10), "clock moved back");
        assert!(!interval_elapsed(DAY * 10, DAY * 10), "same instant");
        assert!(
            !interval_elapsed(DAY * 10 - 1, DAY * 10),
            "inside the window"
        );
        assert!(interval_elapsed(DAY * 9, DAY * 10), "a full day on");
    }

    #[test]
    fn only_a_new_available_release_raises_the_notice() {
        assert!(notice_due(&available("v0.2.0"), "").is_some());
        assert!(notice_due(&available("v0.2.0"), "v0.1.0").is_some());
    }

    #[test]
    fn an_already_notified_tag_stays_quiet() {
        assert!(notice_due(&available("v0.2.0"), "v0.2.0").is_none());
    }

    #[test]
    fn a_later_release_re_arms_the_notice() {
        assert!(notice_due(&available("v0.3.0"), "v0.2.0").is_some());
    }

    #[test]
    fn nothing_else_raises_the_notice() {
        assert!(notice_due(&ReleaseStatus::Pending, "").is_none());
        assert!(notice_due(&ReleaseStatus::Failed, "").is_none());
        assert!(notice_due(
            &ReleaseStatus::UpToDate {
                tag: "v0.1.0".to_owned()
            },
            ""
        )
        .is_none());
        // Uncomparable is as silent as up-to-date: nothing nags off a guess.
        assert!(notice_due(
            &ReleaseStatus::Inconclusive {
                tag: "v0.2.0-rc1".to_owned()
            },
            ""
        )
        .is_none());
    }
}
