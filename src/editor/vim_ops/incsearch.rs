//! Live `/` / `?` incremental search — vim's `incsearch`.
//!
//! A navigate-only [`SearchState`] is rebuilt from the command-line text on every
//! keystroke and the cursor parks on the cursor-relative first match.  *Both* Esc and
//! Enter restore the pre-prompt cursor, scroll, and prior hlsearch session, so the
//! App-level `EnterSearch` path runs against the original view and submit stays
//! byte-identical to a preview-less one (`vim_ops::preview` promises the same).
//!
//! Unlike the `:s` preview, incsearch never touches the buffer, so there is no revert
//! delta and none of the App-level gates apply.

use crate::editor::EditorState;
use crate::search::SearchState;

/// State saved on the first keystroke of an open `/` / `?` prompt, restored when it ends.
/// Lives on `VimState` — its lifetime is bounded by the command line's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncsearchSession {
    /// Restored when the prompt closes: vim keeps the previous highlights on abort.
    prior: Option<SearchState>,
    saved_cursor: usize,
    saved_scroll: usize,
}

/// Re-derive the live search from the command-line text, starting the session on the
/// first call.  An empty or matchless input clears highlights and returns the view to the
/// origin but keeps the session alive.  The focus resolves against the *saved* cursor, so
/// the parked cursor never feeds back into the next keystroke.
pub fn update_incsearch(
    editor: &mut EditorState,
    session: &mut Option<IncsearchSession>,
    input: &str,
    forward: bool,
    viewport_height: usize,
    viewport_width: usize,
) {
    if session.is_none() {
        *session = Some(IncsearchSession {
            prior: editor.search.take(),
            saved_cursor: editor.cursor.offset,
            saved_scroll: editor.scroll,
        });
    }
    let (saved_cursor, saved_scroll) = {
        let s = session.as_ref().expect("session ensured above");
        (s.saved_cursor, s.saved_scroll)
    };
    // A half-typed escape (`/a\`) lands here too: no flash, since the user is still
    // typing — the error is reported on submit.
    let Ok(mut state) = SearchState::new(input.to_owned(), None) else {
        editor.search = None;
        editor.restore_view(saved_cursor, Some(saved_scroll));
        return;
    };
    state.ensure_fresh(&editor.buffer.contents(), editor.buffer.version());
    if state.matches.is_empty() {
        editor.search = None;
        editor.restore_view(saved_cursor, Some(saved_scroll));
        return;
    }
    let cursor_byte = editor
        .buffer
        .rope()
        .char_to_byte(saved_cursor.min(editor.buffer.len_chars()));
    state.focus_relative_to(cursor_byte, forward);
    editor.search = Some(state);
    editor.sync_cursor_to_search_focus();
    editor.scroll_cursor_comfortably_into_view(viewport_height, viewport_width);
}

/// Restore the prior hlsearch session and the pre-prompt cursor and scroll; `true` when a
/// session existed.  Called on both Esc and Enter — see the module docs.
pub fn end_incsearch(editor: &mut EditorState, session: &mut Option<IncsearchSession>) -> bool {
    let Some(s) = session.take() else {
        return false;
    };
    editor.search = s.prior;
    editor.restore_view(s.saved_cursor, Some(s.saved_scroll));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::Buffer;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn editor(text: &str) -> EditorState {
        let mut st = EditorState::new(Buffer::from_str(text), theme());
        st.update_cursor_block();
        st
    }

    #[test]
    fn update_focuses_the_first_match_after_the_origin() {
        let mut st = editor("foo bar\nfoo");
        let mut session = None;
        update_incsearch(&mut st, &mut session, "foo", true, 24, 80);
        let s = st.search.as_ref().expect("session live");
        // The match at byte 0 starts *at* the cursor, so forward search takes the next.
        assert_eq!(s.focused_range(), Some(8..11));
        assert_eq!(st.cursor.offset, 8, "cursor parked on the focus");
    }

    #[test]
    fn backward_search_focuses_the_last_match_before_the_origin() {
        let mut st = editor("foo bar\nfoo");
        st.place_cursor(8);
        let mut session = None;
        update_incsearch(&mut st, &mut session, "foo", false, 24, 80);
        let s = st.search.as_ref().expect("session live");
        assert_eq!(s.focused_range(), Some(0..3));
    }

    #[test]
    fn every_keystroke_resolves_from_the_saved_cursor_not_the_parked_one() {
        let mut st = editor("aa ab ac");
        let mut session = None;
        update_incsearch(&mut st, &mut session, "a", true, 24, 80);
        assert_eq!(st.search.as_ref().unwrap().focused_range(), Some(1..2));
        // Narrowing must resolve from the original cursor (0), not the parked one.
        update_incsearch(&mut st, &mut session, "ab", true, 24, 80);
        assert_eq!(st.search.as_ref().unwrap().focused_range(), Some(3..5));
    }

    #[test]
    fn a_matchless_input_clears_highlights_and_restores_the_view() {
        let mut st = editor("foo bar");
        st.scroll = 0;
        let mut session = None;
        update_incsearch(&mut st, &mut session, "bar", true, 24, 80);
        assert!(st.search.is_some());
        update_incsearch(&mut st, &mut session, "barz", true, 24, 80);
        assert!(st.search.is_none(), "no match → no highlights");
        assert_eq!(st.cursor.offset, 0, "view back at the origin");
        assert!(session.is_some(), "the session survives for later keys");
        update_incsearch(&mut st, &mut session, "bar", true, 24, 80);
        assert!(st.search.is_some());
    }

    #[test]
    fn end_restores_prior_session_cursor_and_scroll() {
        let mut st = editor("foo bar\nfoo");
        let mut prior = SearchState::new("bar".to_owned(), None).expect("valid");
        prior.ensure_fresh(&st.buffer.contents(), st.buffer.version());
        st.search = Some(prior.clone());
        st.place_cursor(5);
        st.scroll = 1;
        let mut session = None;
        update_incsearch(&mut st, &mut session, "foo", true, 24, 80);
        assert_eq!(st.search.as_ref().unwrap().query, "foo");
        assert!(end_incsearch(&mut st, &mut session));
        assert_eq!(
            st.search.as_ref().map(|s| s.query.as_str()),
            Some("bar"),
            "prior hlsearch session restored"
        );
        assert_eq!(st.cursor.offset, 5);
        assert_eq!(st.scroll, 1);
        assert!(!end_incsearch(&mut st, &mut session), "already ended");
    }
}
