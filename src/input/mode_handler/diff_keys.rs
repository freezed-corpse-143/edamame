//! Single source of truth for the diff-review key bindings, which bypass the runtime
//! `KeyMap` so they win over the global keymap (`Tab` → `InsertTab`). Behavior
//! ([`diff_action_for`]) and every UI glyph ([`diff_hint`]: hint bar, overlay, divider,
//! intro modal) derive from `DIFF_REVIEW_BINDINGS`, so the two can never drift.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::Action;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModMatch {
    Exact(KeyModifiers),
    /// Modifier-independent: `Esc`, and `BackTab`, which some terminals report with the
    /// `SHIFT` flag and some without.
    Any,
}

struct DiffBinding {
    key: KeyCode,
    mods: ModMatch,
    action: Action,
    /// Display glyph; empty marks an alias row the behavior path honors but the UI must
    /// not list a second time.
    glyph: &'static str,
}

/// Ordered for first-match-wins; the canonical glyph for an action is its first non-empty row.
const DIFF_REVIEW_BINDINGS: &[DiffBinding] = &[
    DiffBinding {
        key: KeyCode::Esc,
        mods: ModMatch::Any,
        action: Action::DiffExit,
        glyph: "Esc",
    },
    DiffBinding {
        key: KeyCode::Tab,
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::DiffNext,
        glyph: "Tab",
    },
    DiffBinding {
        key: KeyCode::BackTab,
        mods: ModMatch::Any,
        action: Action::DiffPrev,
        glyph: "⇧Tab",
    },
    // Alias: terminals that report Shift-Tab as `Tab + SHIFT`.
    DiffBinding {
        key: KeyCode::Tab,
        mods: ModMatch::Exact(KeyModifiers::SHIFT),
        action: Action::DiffPrev,
        glyph: "",
    },
    DiffBinding {
        key: KeyCode::Char('y'),
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::DiffAcceptHunk,
        glyph: "y",
    },
    DiffBinding {
        key: KeyCode::Char('n'),
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::DiffRejectHunk,
        glyph: "n",
    },
    DiffBinding {
        key: KeyCode::Char('Y'),
        mods: ModMatch::Exact(KeyModifiers::SHIFT),
        action: Action::DiffAcceptAll,
        glyph: "Y",
    },
    DiffBinding {
        key: KeyCode::Char('N'),
        mods: ModMatch::Exact(KeyModifiers::SHIFT),
        action: Action::DiffRejectAll,
        glyph: "N",
    },
    DiffBinding {
        key: KeyCode::Backspace,
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::DiffResetHunk,
        glyph: "⌫",
    },
];

impl ModMatch {
    fn matches(self, event_mods: KeyModifiers) -> bool {
        match self {
            ModMatch::Exact(m) => event_mods == m,
            ModMatch::Any => true,
        }
    }
}

/// The diff-review [`Action`] for a key event; `None` falls through to the global keymap.
pub fn diff_action_for(event: &KeyEvent) -> Option<Action> {
    DIFF_REVIEW_BINDINGS
        .iter()
        .find(|b| b.key == event.code && b.mods.matches(event.modifiers))
        .map(|b| b.action.clone())
}

/// Canonical display glyph for `action`, or `""` when it has no review binding.
pub fn diff_hint(action: &Action) -> &'static str {
    DIFF_REVIEW_BINDINGS
        .iter()
        .find(|b| &b.action == action && !b.glyph.is_empty())
        .map(|b| b.glyph)
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn replaying_each_binding_yields_its_action() {
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        assert_eq!(
            diff_action_for(&ev(KeyCode::Esc, none)),
            Some(Action::DiffExit)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Esc, KeyModifiers::CONTROL)),
            Some(Action::DiffExit)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Tab, none)),
            Some(Action::DiffNext)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::BackTab, shift)),
            Some(Action::DiffPrev)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Tab, shift)),
            Some(Action::DiffPrev)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Char('y'), none)),
            Some(Action::DiffAcceptHunk)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Char('n'), none)),
            Some(Action::DiffRejectHunk)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Char('Y'), shift)),
            Some(Action::DiffAcceptAll)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Char('N'), shift)),
            Some(Action::DiffRejectAll)
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Backspace, none)),
            Some(Action::DiffResetHunk)
        );
    }

    /// `i` / `Enter` once entered an unimplemented Edit sub-mode; binding them again means
    /// implementing the feature first.
    #[test]
    fn edit_sub_mode_keys_are_unbound() {
        let none = KeyModifiers::NONE;
        assert_eq!(diff_action_for(&ev(KeyCode::Char('i'), none)), None);
        assert_eq!(diff_action_for(&ev(KeyCode::Enter, none)), None);
    }

    #[test]
    fn unbound_key_returns_none() {
        assert_eq!(
            diff_action_for(&ev(KeyCode::Char('x'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            diff_action_for(&ev(KeyCode::Char('Y'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn hints_resolve_to_canonical_glyph() {
        assert_eq!(diff_hint(&Action::DiffAcceptHunk), "y");
        assert_eq!(diff_hint(&Action::DiffRejectHunk), "n");
        assert_eq!(diff_hint(&Action::DiffAcceptAll), "Y");
        assert_eq!(diff_hint(&Action::DiffRejectAll), "N");
        assert_eq!(diff_hint(&Action::DiffNext), "Tab");
        assert_eq!(diff_hint(&Action::DiffPrev), "⇧Tab");
        assert_eq!(diff_hint(&Action::DiffResetHunk), "⌫");
        assert_eq!(diff_hint(&Action::DiffExit), "Esc");
        assert_eq!(diff_hint(&Action::Quit), "");
    }
}
