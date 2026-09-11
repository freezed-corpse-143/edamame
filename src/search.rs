//! Search-and-replace: [`SearchState`] (owned by `EditorState::search` while a flow is
//! active) plus a hard-bound key table ([`search_keys`]) that wins over the user keymap.
//! An active search does not change `EditorState::mode`; the flow is gated on
//! `search.is_some()` and `app::actions::search_safe_action` default-denies other actions.
//! Matching is literal substring, never regex, with backslash [`escape`]s for `\n` etc.
//! See `docs/dev/search-replace.md`.

pub mod escape;
pub mod search_keys;
pub mod state;

pub use escape::EscapeError;
pub use search_keys::{search_action_for, search_hint};
pub use state::{SearchError, SearchState};
