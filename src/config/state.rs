//! Machine-written persistent state — `state.toml` in the data directory.
//!
//! Distinct from [`Config`]: these values are bookkeeping edamame writes
//! on its own (which terminals it has profiled, when it last checked for updates), never settings
//! the user hand-edits.  They lived in `config.toml`'s `[editor]` table until they cluttered a
//! file meant for humans — and one of them, [`State::seen_terminal_fingerprints`], is inherently
//! per-machine, so it had no business in a `config.toml` symlinked across machines from a dotfiles
//! repo.  [`Config::load`](super::config::Config::load) migrates an existing `config.toml` by
//! seeding this file once and stripping the four keys.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::config::Config;
use super::persistence::{config_reads_allowed, config_writes_allowed};

/// Machine-written bookkeeping persisted to `state.toml`, separate from the user-facing
/// `config.toml`.  Every field is written by edamame, never hand-edited; a missing or partial
/// file loads as defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Terminals the capabilities notice has already fired for.  Built by
    /// [`crate::terminal::Capabilities::fingerprint`]; an unseen fingerprint re-fires the notice.
    pub seen_terminal_fingerprints: Vec<String>,
    /// Unix epoch seconds of the last automatic release check, stamped when the check is
    /// *spawned*, so a hung worker or a killed process can't re-check on every launch.  `0` means
    /// never checked.
    pub last_update_check: u64,
    /// Release tag the startup notice has already fired for.
    pub update_notified_for: String,
    /// The version that last ran, driving the one-time post-upgrade notes (`app::post_upgrade`);
    /// no network involved.  Empty covers both a fresh install and an upgrade from a build
    /// predating the field — `show_welcome` tells them apart, since only a returning user could
    /// have turned it off.
    pub last_version_seen: String,
    /// IDs of the daily tips (`app::tips`) already shown, so no tip repeats.  A `Vec`, not a set:
    /// TOML has no set type, the list stays small, and a stable order keeps the machine file's
    /// diffs quiet.
    pub seen_daily_tips: Vec<u32>,
    /// Unix epoch seconds the last daily tip was shown, stamped when it fires; `0` means never.
    /// The daily-tip gate reads it exactly as the update check reads [`Self::last_update_check`].
    pub last_tip_shown: u64,
    /// Where the cursor sat in a file when the session that last left it ended, so reopening the
    /// file resumes there.  See [`State::remember_cursor`] for the order and the cap.
    pub cursors: Vec<RememberedCursor>,
}

/// Ceiling on [`State::cursors`].  Past it the least recently written entry is dropped, so the
/// machine file stays bounded however many documents the user opens; an evicted file simply
/// reopens at the top.
pub const MAX_REMEMBERED_CURSORS: usize = 50;

/// One file's remembered cursor, as [`State::cursors`] holds it.
///
/// `path` is a [`cursor_key`], not the user's spelling, so `edamame notes.md` and an absolute
/// rerun resolve to the same entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberedCursor {
    pub path: String,
    /// Char offset into the rope — [`crate::document::Cursor::offset`]'s unit, not bytes — clamped
    /// to the buffer on restore, so a file rewritten shorter lands at its end rather than panicking.
    pub offset: usize,
}

/// The key a path is remembered under: the canonical path when the file exists, else the path as
/// given.  Canonicalizing is what makes a relative open and an absolute one agree; the fallback
/// keeps a path with no file behind it (a new buffer, a deleted one) usable rather than dropping
/// the entry.
pub fn cursor_key(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

impl State {
    // ── Remembered cursors ────────────────────────────────────────────────

    /// The cursor remembered for `key`, if that file has one.
    pub fn cursor_for(&self, key: &str) -> Option<usize> {
        self.cursors
            .iter()
            .find(|c| c.path == key)
            .map(|c| c.offset)
    }

    /// Remember `offset` as the cursor for `key`.
    ///
    /// The entry moves to the end, because *write* order is the recency order the cap evicts by —
    /// cheaper than a timestamp, and the file's diff stays one line wide.  Writing is the caller's
    /// job ([`crate::app::App::record_cursor_position`]); this stays a pure in-memory edit like
    /// every other method here.
    pub fn remember_cursor(&mut self, key: String, offset: usize) {
        self.cursors.retain(|c| c.path != key);
        self.cursors.push(RememberedCursor { path: key, offset });
        let excess = self.cursors.len().saturating_sub(MAX_REMEMBERED_CURSORS);
        self.cursors.drain(..excess);
    }

    /// Path to `state.toml` in the data directory (may not exist yet); `None` when the data
    /// directory can't be resolved.  Shares its base with [`Config::log_dir`](Config::log_dir).
    pub fn path() -> Option<PathBuf> {
        Config::data_dir().map(|d| d.join("state.toml"))
    }

    /// Read `state.toml`, defaulting for anything missing or unreadable.
    ///
    /// Fail-soft with no warning modal: this is a machine file, not user-authored, so a parse
    /// error is edamame's own bug — logged, then treated as "no state recorded".  A `--no-config`
    /// run reads nothing and returns defaults.
    pub fn load() -> State {
        if !config_reads_allowed() {
            return State::default();
        }
        let Some(path) = Self::path() else {
            return State::default();
        };
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return State::default(),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "failed to read state.toml");
                return State::default();
            }
        };
        match toml::from_str(&raw) {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "failed to parse state.toml");
                State::default()
            }
        }
    }

    /// Persist to `state.toml`.  A plain serialize — the file is machine-only, so there are no
    /// comments to preserve and no merge to perform (unlike [`Config::save`](Config::save)).
    ///
    /// A `--no-config` session returns `Ok(())` without writing, matching `Config::save`.
    pub fn save(&self) -> Result<()> {
        if !config_writes_allowed() {
            return Ok(());
        }
        let path = Self::path()
            .context("Could not determine data directory (missing XDG_DATA_HOME/HOME)")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create data directory: {}", parent.display())
            })?;
        }
        let output = toml::to_string_pretty(self).context("Failed to serialize state to TOML")?;
        std::fs::write(&path, output)
            .with_context(|| format!("Failed to write state file: {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_defaults_and_round_trips() {
        let state = State::default();
        assert!(state.seen_terminal_fingerprints.is_empty());
        assert_eq!(state.last_update_check, 0);
        assert_eq!(state.update_notified_for, "");
        // Empty means "no version recorded", which `App::new` reads with `show_welcome` to tell a
        // fresh install from an upgrade.
        assert_eq!(state.last_version_seen, "");

        assert!(state.seen_daily_tips.is_empty());
        assert_eq!(state.last_tip_shown, 0);
        assert!(state.cursors.is_empty());

        let state = State {
            seen_terminal_fingerprints: vec![
                "WezTerm|xterm-256color||truecolor|kitty|mouse=true|kbd=true|unicode=true".into(),
            ],
            last_update_check: 1_755_500_000,
            update_notified_for: "v0.2.0".to_owned(),
            last_version_seen: "0.1.9".to_owned(),
            seen_daily_tips: vec![1, 2],
            last_tip_shown: 1_755_600_000,
            cursors: vec![RememberedCursor {
                path: "/tmp/notes.md".to_owned(),
                offset: 42,
            }],
        };
        let serialized = toml::to_string_pretty(&state).expect("serialize");
        let deserialized: State = toml::from_str(&serialized).expect("deserialize");
        assert_eq!(
            deserialized.seen_terminal_fingerprints,
            state.seen_terminal_fingerprints
        );
        assert_eq!(deserialized.last_update_check, 1_755_500_000);
        assert_eq!(deserialized.update_notified_for, "v0.2.0");
        assert_eq!(deserialized.last_version_seen, "0.1.9");
        assert_eq!(deserialized.seen_daily_tips, vec![1, 2]);
        assert_eq!(deserialized.last_tip_shown, 1_755_600_000);
        assert_eq!(deserialized.cursors, state.cursors);
        assert_eq!(deserialized.cursor_for("/tmp/notes.md"), Some(42));
    }

    /// A partial file still loads: a `state.toml` from an older build missing a field defaults it
    /// rather than erroring.
    #[test]
    fn partial_state_falls_back_to_defaults() {
        let state: State = toml::from_str("last_update_check = 42\n").expect("deserialize");
        assert_eq!(state.last_update_check, 42);
        assert!(state.seen_terminal_fingerprints.is_empty());
        assert_eq!(state.last_version_seen, "");
    }

    #[test]
    fn path_lives_in_the_data_dir() {
        // `path()` is `None` only when no home/data dir resolves; on a normal dev/CI machine it
        // ends with `edamame/state.toml`.
        if let Some(path) = State::path() {
            assert!(path.ends_with("edamame/state.toml"), "{path:?}");
        }
    }

    /// A suppressed session (`--no-config`, test isolation) writes nothing and reads defaults.
    #[test]
    fn save_and_load_are_no_ops_under_no_config() {
        let _lock = crate::test_env::env_lock();
        let state = State {
            last_version_seen: "9.9.9".to_owned(),
            ..State::default()
        };
        let _suppressed = crate::config::persistence::SuppressGuard::new();
        assert!(state.save().is_ok(), "a suppressed save is not a failure");
        assert_eq!(State::load().last_version_seen, "", "load reads nothing");
    }

    /// The real read/write round-trip, kept to Linux where `dirs::data_dir()` honors
    /// `XDG_DATA_HOME`; elsewhere it ignores the override and would touch the real data dir.  This
    /// is the counterpart proving the no-op test above is about the gate, not a misdirected path.
    #[cfg(target_os = "linux")]
    #[test]
    fn save_then_load_round_trips_through_disk() {
        let _lock = crate::test_env::env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _xdg = crate::test_env::EnvGuard::set("XDG_DATA_HOME", dir.path());

        let state = State {
            last_version_seen: "9.9.9".to_owned(),
            ..State::default()
        };
        state.save().expect("save ok");
        assert!(dir.path().join("edamame/state.toml").exists());
        assert_eq!(State::load().last_version_seen, "9.9.9");
    }

    /// Write order *is* the recency order, so re-remembering a file moves it to the end.
    #[test]
    fn remembering_a_file_again_moves_it_to_the_end() {
        let mut state = State::default();
        state.remember_cursor("a".to_owned(), 1);
        state.remember_cursor("b".to_owned(), 2);
        state.remember_cursor("a".to_owned(), 3);

        assert_eq!(state.cursor_for("a"), Some(3), "the latest offset wins");
        assert_eq!(state.cursor_for("b"), Some(2));
        assert_eq!(
            state
                .cursors
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"],
            "the entry just written sits last, where the cap evicts from the front"
        );
    }

    /// Past the cap the oldest write is dropped and the newest kept.
    #[test]
    fn remembering_evicts_the_oldest_past_the_cap() {
        let mut state = State::default();
        for i in 0..MAX_REMEMBERED_CURSORS + 5 {
            state.remember_cursor(format!("file{i}"), i);
        }

        assert_eq!(state.cursors.len(), MAX_REMEMBERED_CURSORS);
        assert_eq!(state.cursor_for("file4"), None, "the oldest five are gone");
        assert_eq!(state.cursor_for("file5"), Some(5));
    }

    /// Two spellings of one file resolve to one entry; a path with no file behind it still keys.
    #[test]
    fn cursor_key_canonicalizes_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.md");
        std::fs::write(&path, "x\n").unwrap();

        let roundabout = dir.path().join(".").join("notes.md");
        assert_eq!(
            cursor_key(&roundabout),
            cursor_key(&path),
            "a roundabout spelling is the same file"
        );

        let missing = dir.path().join("nope.md");
        assert!(
            cursor_key(&missing).ends_with("nope.md"),
            "an absent file still keys on its path instead of panicking"
        );
    }
}
