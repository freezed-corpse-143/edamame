//! The one-time post-upgrade notice: this build is new, here is what changed in it.  See
//! docs/dev/post-upgrade.md.
//!
//! Distinct from [`super::update_check`], which asks whether a *newer* release exists on GitHub;
//! this asks whether *this* build is newer than the one that last ran, and reads the answer from
//! the compiled-in changelog.  No network, so it decides synchronously inside `App::new` and
//! joins the startup modal ordering directly.
//!
//! **The decision happens in `App::new`; the write does not.** Most tests reach `App::new`
//! without a `config_isolation()` guard, so a `Config::save` there would rewrite the developer's
//! own `config.toml`.  [`App::stamp_last_version_seen`] therefore runs from `App::run`.

pub(crate) mod changelog;

use super::modal;
use super::update_check::INSTALLED_VERSION;
use super::App;
use crate::config::persistence;

/// What this launch owes the user about the version it is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PostUpgradeAction {
    /// The recorded version is the running one: nothing happened.
    Nothing,
    /// A first run: record the version so the *next* upgrade is recognizable, but say nothing —
    /// there is no "what's new" for a user who has run nothing else.
    StampSilently,
    /// The build changed under a user who has been here before.
    Show,
}

/// Decide what an upgrade is owed, as a pure function of primitives so it is table-testable
/// without an `App`.
///
/// An **empty** `last_version_seen` is ambiguous: both a fresh install and an upgrade from a
/// build predating the field look that way.  `show_welcome` disambiguates, because only somebody
/// who has been here before could have turned it off.
pub(crate) fn post_upgrade_action(
    last_version_seen: &str,
    installed: &str,
    show_welcome: bool,
) -> PostUpgradeAction {
    if last_version_seen == installed {
        return PostUpgradeAction::Nothing;
    }
    if last_version_seen.is_empty() && show_welcome {
        return PostUpgradeAction::StampSilently;
    }
    PostUpgradeAction::Show
}

/// Build the startup notice, if this launch is owed one.  A free function because `App::new`
/// calls it while still assembling itself.
///
/// Refused outright when config writes are suppressed (`--no-config`): the stamp is what makes
/// the notice one-time, so without it the same modal would rise on every launch.
pub(crate) fn startup_notice(
    last_version_seen: &str,
    show_welcome: bool,
) -> Option<modal::PostUpgradeModal> {
    if !persistence::config_writes_allowed() {
        return None;
    }
    match post_upgrade_action(last_version_seen, INSTALLED_VERSION, show_welcome) {
        PostUpgradeAction::Show => modal::PostUpgradeModal::for_upgrade(),
        PostUpgradeAction::Nothing | PostUpgradeAction::StampSilently => None,
    }
}

impl App {
    /// Record the version this session is running, so the notice fires once per upgrade.
    ///
    /// Called from `App::run`, not `App::new` — see the module doc.  Stamps regardless of
    /// whether a modal was shown: a release without a changelog section is silent, and leaving
    /// it unrecorded would re-evaluate it on every later launch.  Needs no `--no-config` gate;
    /// `Config::save` already declines there.
    pub(super) fn stamp_last_version_seen(&mut self) {
        if self.config.editor.last_version_seen == INSTALLED_VERSION {
            return;
        }
        self.config.editor.last_version_seen = INSTALLED_VERSION.to_owned();
        self.save_update_bookkeeping("last-version-seen");
    }

    /// Open the release notes on demand — the About page's `[ Release notes ]` button.
    ///
    /// Always shows the modal, even without a changelog section for this version: a question the
    /// user just asked gets answered.  Reads and writes no bookkeeping — looking is not being
    /// notified, so this neither arms nor disarms the startup notice.
    pub fn open_post_upgrade_modal(&mut self) {
        if self.modal_stack.contains::<modal::PostUpgradeModal>() {
            return;
        }
        self.modal_stack
            .push(Box::new(modal::PostUpgradeModal::on_demand()));
        self.needs_draw = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_utils::make_app;

    const OLDER: &str = "0.0.1";

    #[test]
    fn the_running_version_owes_nothing() {
        assert_eq!(
            post_upgrade_action(INSTALLED_VERSION, INSTALLED_VERSION, false),
            PostUpgradeAction::Nothing
        );
        // A pending welcome doesn't change it: the version is already recorded.
        assert_eq!(
            post_upgrade_action(INSTALLED_VERSION, INSTALLED_VERSION, true),
            PostUpgradeAction::Nothing
        );
    }

    #[test]
    fn a_fresh_install_is_stamped_but_not_greeted() {
        assert_eq!(
            post_upgrade_action("", INSTALLED_VERSION, true),
            PostUpgradeAction::StampSilently
        );
    }

    #[test]
    fn an_upgrade_from_before_the_field_existed_is_shown() {
        // Same empty string as a fresh install; only `show_welcome` separates them.
        assert_eq!(
            post_upgrade_action("", INSTALLED_VERSION, false),
            PostUpgradeAction::Show
        );
    }

    #[test]
    fn an_ordinary_upgrade_is_shown_whatever_the_welcome_says() {
        assert_eq!(
            post_upgrade_action(OLDER, INSTALLED_VERSION, false),
            PostUpgradeAction::Show
        );
        assert_eq!(
            post_upgrade_action(OLDER, INSTALLED_VERSION, true),
            PostUpgradeAction::Show
        );
    }

    #[test]
    fn a_downgrade_is_shown_too() {
        // A downgrade is still a change of build; not worth a state of its own.
        assert_eq!(
            post_upgrade_action("999.0.0", INSTALLED_VERSION, false),
            PostUpgradeAction::Show
        );
    }

    #[test]
    fn no_config_refuses_the_notice_outright() {
        // Asks `startup_notice` rather than building an App: `config_isolation` sets the same
        // suppression `--no-config` does.
        let _iso = crate::test_env::config_isolation();
        assert!(
            startup_notice("", false).is_none(),
            "writes are suppressed, so nothing may be shown"
        );
    }

    #[test]
    fn stamping_records_the_running_version() {
        let _iso = crate::test_env::config_isolation();
        let mut app = make_app();
        app.config.editor.last_version_seen = OLDER.to_owned();
        app.stamp_last_version_seen();
        assert_eq!(app.config.editor.last_version_seen, INSTALLED_VERSION);
    }

    #[test]
    fn stamping_an_already_current_version_changes_nothing() {
        let _iso = crate::test_env::config_isolation();
        let mut app = make_app();
        app.config.editor.last_version_seen = INSTALLED_VERSION.to_owned();
        app.stamp_last_version_seen();
        assert_eq!(app.config.editor.last_version_seen, INSTALLED_VERSION);
    }

    #[test]
    fn the_explicit_opening_writes_no_bookkeeping() {
        let _iso = crate::test_env::config_isolation();
        let mut app = make_app();
        app.config.editor.last_version_seen = OLDER.to_owned();
        app.open_post_upgrade_modal();
        assert!(app.modal_stack.contains::<modal::PostUpgradeModal>());
        assert_eq!(app.config.editor.last_version_seen, OLDER);
    }

    /// Build an `App` the way a returning user's launch does: welcome dismissed,
    /// `last_version_seen` as given.
    ///
    /// **The caller must hold [`crate::test_env::env_lock`] — and not `config_isolation` — for
    /// the whole test.** `config_isolation` clears the very write gate `startup_notice` reads,
    /// so it would gate away the behavior under test; the bare lock still excludes another test
    /// setting that suppression concurrently.  Safe without it because `App::new` never writes.
    fn returning_user_app(last_version_seen: &str) -> App {
        use crate::config::{Config, KeyBindingOverrides, Theme};
        use crate::terminal::{Capabilities, ColorDepth};

        let caps = Capabilities {
            color_depth: ColorDepth::TrueColor,
            ..Capabilities::default()
        };
        let mut config = Config::default();
        config.editor.show_welcome = false;
        config.editor.last_version_seen = last_version_seen.to_owned();
        App::new(
            config,
            KeyBindingOverrides::default(),
            (&Theme::default()).into(),
            None,
            caps,
            Vec::new(),
        )
        .expect("build app")
    }

    #[test]
    fn a_returning_user_on_a_new_build_is_shown_the_notice_at_startup() {
        // End to end through `App::new` — the one thing the pure policy test can't prove.
        let _lock = crate::test_env::env_lock();
        let app = returning_user_app("0.0.1");
        assert_eq!(
            app.modal_stack.contains::<modal::PostUpgradeModal>(),
            changelog::notes_for_version(INSTALLED_VERSION).is_some(),
            "shown exactly when this version has changelog notes"
        );
    }

    #[test]
    fn a_launch_on_the_recorded_version_raises_nothing() {
        let _lock = crate::test_env::env_lock();
        let app = returning_user_app(INSTALLED_VERSION);
        assert!(!app.modal_stack.contains::<modal::PostUpgradeModal>());
    }

    #[test]
    fn a_first_run_is_never_greeted_with_release_notes() {
        // Default config: `show_welcome` on, no version recorded.  `env_lock` rather than
        // `config_isolation`, or the assertion would hold whatever the rule did.
        let _lock = crate::test_env::env_lock();
        let app = make_app();
        assert!(!app.modal_stack.contains::<modal::PostUpgradeModal>());
    }

    #[test]
    fn opening_it_twice_does_not_stack_it() {
        let _iso = crate::test_env::config_isolation();
        let mut app = make_app();
        app.open_post_upgrade_modal();
        app.open_post_upgrade_modal();
        assert_eq!(app.modal_stack.count::<modal::PostUpgradeModal>(), 1);
    }
}
