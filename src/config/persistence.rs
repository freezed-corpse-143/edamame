//! The single "is `~/.config/edamame` in play at all?" gate behind `--no-config`.
//! See `docs/dev/cli.md`.
//!
//! A process-global rather than a `Config` field: `App::open_config_in_editor` replaces
//! `self.config` with a freshly deserialized one mid-session, which reverted a field to
//! its serde default and silently lapsed the guarantee.
//!
//! Both halves matter.  Every write into the directory asks [`config_writes_allowed`];
//! skipping the *startup* load is not enough for reads, because the theme and export
//! stylesheet listings re-read the directory long after `main` branched, so they ask
//! [`config_reads_allowed`].  A new reader or writer owes the matching check.
//! (`Config::ensure_default_files` is exempt only because `main` never calls it here.)

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether `~/.config/edamame` participates in this run at all.  Starts `true`; only
/// [`disable_config_dir`] ever clears it, and nothing sets it back.
static CONFIG_DIR_IN_USE: AtomicBool = AtomicBool::new(true);

/// What a "saved" message says instead when the write was suppressed.  The setting *is*
/// live for the session; only the disk write was skipped.
pub const NOT_PERSISTED_NOTE: &str = " (not saved: --no-config)";

/// Take `~/.config/edamame` out of play for the rest of the process, in both directions.
/// Called once from `main` before any config file is touched.  There is deliberately no
/// way to re-enable: a mid-session reversal is the bug this design exists to prevent.
pub fn disable_config_dir() {
    CONFIG_DIR_IN_USE.store(false, Ordering::Relaxed);
}

/// The write half of the gate.  `Relaxed` suffices: the value is written once at startup
/// on the main thread, before any other thread exists, and every later access is a read.
pub fn config_writes_allowed() -> bool {
    CONFIG_DIR_IN_USE.load(Ordering::Relaxed)
}

/// The read half of the gate.  Same flag as [`config_writes_allowed`]; separate only so a
/// reader isn't guarded by a function with "writes" in its name.
pub fn config_reads_allowed() -> bool {
    CONFIG_DIR_IN_USE.load(Ordering::Relaxed)
}

/// Suffix for a flash reporting a settings change: empty on an ordinary run,
/// [`NOT_PERSISTED_NOTE`] when writes are suppressed.  Callers phrase the sentence so
/// both readings are true.
pub fn unpersisted_suffix() -> &'static str {
    if config_writes_allowed() {
        ""
    } else {
        NOT_PERSISTED_NOTE
    }
}

/// Test-only scoped suppression, serialized by [`crate::test_env::env_lock`] rather than a
/// mutex of its own.
///
/// **Callers must hold `env_lock` for the whole test**, not just across the guard's
/// lifetime: these tests are shaped "assert nothing was written, drop the guard, assert the
/// same call *does* write", and the second half would fail if another test's guard were
/// live then.
#[cfg(test)]
pub(crate) struct SuppressGuard;

#[cfg(test)]
impl SuppressGuard {
    /// Suppress config reads and writes until the returned guard drops.
    /// Only valid while holding [`crate::test_env::env_lock`].
    pub(crate) fn new() -> Self {
        CONFIG_DIR_IN_USE.store(false, Ordering::Relaxed);
        Self
    }
}

#[cfg(test)]
impl Drop for SuppressGuard {
    fn drop(&mut self) {
        CONFIG_DIR_IN_USE.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_are_allowed_by_default() {
        let _lock = crate::test_env::env_lock();
        assert!(config_writes_allowed());
        assert!(config_reads_allowed());
        assert_eq!(unpersisted_suffix(), "");
    }

    /// The restore matters as much as the suppression: every other test shares the global.
    #[test]
    fn the_guard_suppresses_and_restores_both_halves() {
        let _lock = crate::test_env::env_lock();
        {
            let _g = SuppressGuard::new();
            assert!(!config_writes_allowed());
            assert!(!config_reads_allowed());
            assert_eq!(unpersisted_suffix(), NOT_PERSISTED_NOTE);
        }
        assert!(config_writes_allowed());
        assert!(config_reads_allowed());
    }
}
