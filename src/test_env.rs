//! Test-only helpers for mutating process environment variables.
//!
//! **The lock is crate-wide, deliberately.**  `std::env::set_var` is `unsafe` because it races any
//! concurrent `env::var`, and cargo runs a binary's tests on parallel threads.  A per-module lock
//! can't exclude the real pairs: `config::config` writes `XDG_CONFIG_HOME` while `cli::doctor`
//! reads it, and `terminal::capabilities` writes `TERM_PROGRAM` while `cli::doctor` reads that.
//!
//! Two rules for any test touching the environment:
//!
//! 1. Take [`env_lock`] first and hold it for the whole test — readers included, since a read is
//!    the other half of the same race.
//! 2. Mutate only through [`EnvGuard`], so a panicking assertion can't leak an `XDG_CONFIG_HOME`
//!    pointing at a deleted tempdir and fail every later config test.
//!
//! The same lock serialises [`config::persistence::SuppressGuard`], which flips a process-global
//! `AtomicBool`.  That read side is easy to miss because such a test touches no environment
//! variable of its own: anything observing the config gate — `list_export_stylesheets`,
//! `list_theme_names`, `read_theme_named`, `Config::save` — must take the lock, or it reads the
//! gate while another thread holds it closed and sees an inexplicably empty list.
//!
//! [`config::persistence::SuppressGuard`]: crate::config::persistence::SuppressGuard

use std::env;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Serialises every environment-touching test in the crate.  Poisoning is ignored: [`EnvGuard`]'s
/// `Drop` has already restored the variable by the time a panicking test releases the lock.
pub fn env_lock() -> MutexGuard<'static, ()> {
    static M: OnceLock<Mutex<()>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Sets or clears one environment variable, restoring it on drop.  Create only under [`env_lock`].
pub struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    /// Set `key` to `value` for the guard's lifetime.
    pub fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prev = env::var(key).ok();
        // SAFETY: every env-mutating and env-reading test in this crate
        // holds `env_lock`, so no other thread is in `env::var` here.
        unsafe {
            env::set_var(key, value);
        }
        Self { key, prev }
    }

    /// Remove `key` for the guard's lifetime.
    pub fn unset(key: &'static str) -> Self {
        let prev = env::var(key).ok();
        // SAFETY: as above.
        unsafe {
            env::remove_var(key);
        }
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: as above — the lock is still held by the test whose
        // scope this guard is ending.
        unsafe {
            match &self.prev {
                Some(v) => env::set_var(self.key, v),
                None => env::remove_var(self.key),
            }
        }
    }
}

/// The crate-wide env lock plus suppressed config reads and writes, as one guard.
///
/// **Any test that can reach [`Config::save`] must hold this.**  Nothing redirects
/// `~/.config/edamame` during a test run, so an unguarded save rewrites the *developer's own*
/// config — with exactly the damaging values tests assert (an update-check test recording
/// `v999.0.0` silences the real update notice forever).  Suppressing the gate beats pointing
/// `XDG_CONFIG_HOME` at a tempdir: the write should be gone, not relocated, and `Config::save`
/// still returns `Ok(())`.
///
/// Reads are suppressed alongside writes because the gate is one flag (see
/// [`crate::config::persistence`]).  The one test needing a real write takes [`env_lock`] plus an
/// [`EnvGuard`] on `XDG_CONFIG_HOME` instead.
pub struct ConfigIsolation {
    // Declaration order is drop order: the suppression must lift while the lock is still held, or
    // another test observes the gate mid-restore.
    _suppress: crate::config::persistence::SuppressGuard,
    _lock: MutexGuard<'static, ()>,
}

/// Take [`ConfigIsolation`] for the current scope.  Hold it for the whole test body and take it
/// only once — it is a mutex.
pub fn config_isolation() -> ConfigIsolation {
    let lock = env_lock();
    ConfigIsolation {
        _suppress: crate::config::persistence::SuppressGuard::new(),
        _lock: lock,
    }
}
