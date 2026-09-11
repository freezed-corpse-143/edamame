//! Single source of truth for the search-flow key bindings, which bypass the runtime
//! `KeyMap` so they win over the global keymap (`Tab` → `InsertTab`). Behavior
//! ([`search_action_for`]) and display ([`search_hint`]) both derive from `SEARCH_BINDINGS`,
//! so the advertised chord can never disagree with the key that fires. Mirrors
//! `input::mode_handler::diff_keys`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::Action;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModMatch {
    Exact(KeyModifiers),
    /// Modifier-independent: `Esc`, and `BackTab`, which some terminals report with the
    /// `SHIFT` flag and some without.
    Any,
}

struct SearchBinding {
    key: KeyCode,
    mods: ModMatch,
    action: Action,
    /// Display glyph; empty marks an alias row the behavior path honors but the UI must
    /// not list a second time.
    glyph: &'static str,
}

/// Ordered for first-match-wins; the canonical glyph for an action is its first non-empty row.
const SEARCH_BINDINGS: &[SearchBinding] = &[
    SearchBinding {
        key: KeyCode::Esc,
        mods: ModMatch::Any,
        action: Action::SearchExit,
        glyph: "Esc",
    },
    SearchBinding {
        key: KeyCode::Tab,
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::SearchNext,
        glyph: "Tab",
    },
    SearchBinding {
        key: KeyCode::BackTab,
        mods: ModMatch::Any,
        action: Action::SearchPrev,
        glyph: "⇧Tab",
    },
    // Alias: terminals that report Shift-Tab as `Tab + SHIFT`.
    SearchBinding {
        key: KeyCode::Tab,
        mods: ModMatch::Exact(KeyModifiers::SHIFT),
        action: Action::SearchPrev,
        glyph: "",
    },
    SearchBinding {
        key: KeyCode::Char('r'),
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::SearchReplace,
        glyph: "r",
    },
    SearchBinding {
        key: KeyCode::Char('a'),
        mods: ModMatch::Exact(KeyModifiers::NONE),
        action: Action::SearchReplaceAll,
        glyph: "a",
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

/// The search-flow [`Action`] for a key event; `None` falls through to the global keymap.
pub fn search_action_for(event: &KeyEvent) -> Option<Action> {
    SEARCH_BINDINGS
        .iter()
        .find(|b| b.key == event.code && b.mods.matches(event.modifiers))
        .map(|b| b.action.clone())
}

/// Canonical display glyph for `action`, or `""` when it has no flow binding.
pub fn search_hint(action: &Action) -> &'static str {
    SEARCH_BINDINGS
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
            search_action_for(&ev(KeyCode::Esc, none)),
            Some(Action::SearchExit)
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::Esc, KeyModifiers::CONTROL)),
            Some(Action::SearchExit)
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::Tab, none)),
            Some(Action::SearchNext)
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::BackTab, shift)),
            Some(Action::SearchPrev)
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::Tab, shift)),
            Some(Action::SearchPrev)
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::Char('r'), none)),
            Some(Action::SearchReplace)
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::Char('a'), none)),
            Some(Action::SearchReplaceAll)
        );
    }

    #[test]
    fn unbound_keys_fall_through_to_the_global_keymap() {
        assert_eq!(
            search_action_for(&ev(KeyCode::Char('x'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            search_action_for(&ev(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn hints_resolve_to_canonical_glyphs() {
        assert_eq!(search_hint(&Action::SearchNext), "Tab");
        assert_eq!(search_hint(&Action::SearchPrev), "⇧Tab");

        assert_eq!(search_hint(&Action::SearchReplace), "r");
        assert_eq!(search_hint(&Action::SearchReplaceAll), "a");
        assert_eq!(search_hint(&Action::SearchExit), "Esc");
        assert_eq!(search_hint(&Action::OpenSearch), "");
    }
}
