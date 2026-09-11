//! Session state for an active search-and-replace flow.

use std::ops::Range;

use super::escape::{self, EscapeError};

/// Why a search session couldn't be built from the user's input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    #[error("Search term cannot be empty")]
    Empty,
    #[error("{0}")]
    Escape(#[from] EscapeError),
}

/// The active search (and optionally replace) session, owned by `EditorState::search`.
///
/// Match ranges are byte offsets valid only for the buffer version they were computed against, so
/// callers must run [`Self::ensure_fresh`] after any buffer mutation; the render layer also clamps
/// every range against the live source so a missed refresh can't panic.
///
/// **A match may span a line break** (`/  \n`), so every consumer of [`Self::matches`] must clip
/// each range against the line it is painting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchState {
    /// The query **as the user typed it**, escapes and all — the display form, so it can never
    /// carry a raw newline into a single-row surface.  Never empty; match with [`Self::needle`].
    pub query: String,
    /// Replacement text as typed.  `None` is a navigate-only flow, with no Replace keys.
    pub replace: Option<String>,
    /// [`Self::query`] with its escapes decoded: the literal text searched for.  Matched smartcase
    /// for navigation, case-sensitively for a replace flow — see [`Self::ensure_fresh`].
    pub needle: String,
    /// [`Self::replace`] with its escapes decoded.
    pub replacement: Option<String>,
    /// Non-overlapping match byte ranges in document order.
    pub matches: Vec<Range<usize>>,
    /// Index into [`Self::matches`] of the emphasized current match.
    pub focused_idx: usize,
    /// `Buffer::version()` the match list was computed against.
    buffer_version: u64,
}

impl SearchState {
    /// Build a session from the raw text the user typed, decoding its backslash escapes into the
    /// literal needle.  Fails on an empty query or a malformed escape.  The match list starts stale;
    /// the caller's first [`Self::ensure_fresh`] populates it.
    pub fn new(query: String, replace: Option<String>) -> Result<Self, SearchError> {
        if query.is_empty() {
            return Err(SearchError::Empty);
        }
        let needle = escape::decode(&query)?;
        if needle.is_empty() {
            return Err(SearchError::Empty);
        }
        let replacement = replace.as_deref().map(escape::decode).transpose()?;
        Ok(Self {
            query,
            replace,
            needle,
            replacement,
            matches: Vec::new(),
            focused_idx: 0,
            // Forces the first `ensure_fresh` to compute.
            buffer_version: u64::MAX,
        })
    }

    /// True when a replacement was provided — enables the Replace keys and their hint chords.
    pub fn is_replace_flow(&self) -> bool {
        self.replace.is_some()
    }

    /// True when the match list was computed against `version`.  Callers check this first to avoid
    /// the O(n) rope-to-`String` copy [`Self::ensure_fresh`]'s `&str` argument would cost.
    pub fn is_fresh(&self, version: u64) -> bool {
        self.buffer_version == version
    }

    /// Recompute the match list when `version` differs from the one it was built against, wrapping
    /// an out-of-range `focused_idx` to the first match.  Returns true when a recompute happened.
    pub fn ensure_fresh(&mut self, source: &str, version: u64) -> bool {
        if self.buffer_version == version {
            return false;
        }
        // Smartcase is navigation-only: a replace flow stays case-sensitive so it never rewrites
        // a casing variant the user didn't type.
        self.matches = if self.is_replace_flow() {
            find_all_cs(source, &self.needle)
        } else {
            find_all(source, &self.needle)
        };
        self.buffer_version = version;
        if self.focused_idx >= self.matches.len() {
            self.focused_idx = 0;
        }
        true
    }

    /// Byte range of the current match, if any matches remain.
    pub fn focused_range(&self) -> Option<Range<usize>> {
        self.matches.get(self.focused_idx).cloned()
    }

    /// Move focus to the next match, wrapping at the end.
    pub fn advance_focus(&mut self) {
        if !self.matches.is_empty() {
            self.focused_idx = (self.focused_idx + 1) % self.matches.len();
        }
    }

    /// Move focus to the previous match, wrapping at the start.
    pub fn retreat_focus(&mut self) {
        if !self.matches.is_empty() {
            self.focused_idx = self
                .focused_idx
                .checked_sub(1)
                .unwrap_or(self.matches.len() - 1);
        }
    }

    /// Focus the first match strictly after `cursor_byte` (forward) or the last strictly before it
    /// (backward), wrapping around the document — vim's `/` / `?` initial-focus semantics.
    pub fn focus_relative_to(&mut self, cursor_byte: usize, forward: bool) {
        if self.matches.is_empty() {
            return;
        }
        self.focused_idx = if forward {
            let i = self.matches.partition_point(|m| m.start <= cursor_byte);
            if i >= self.matches.len() {
                0
            } else {
                i
            }
        } else {
            let i = self.matches.partition_point(|m| m.start < cursor_byte);
            if i == 0 {
                self.matches.len() - 1
            } else {
                i - 1
            }
        };
    }
}

/// The **navigation** matcher (`/`, `n`/`N`, `Ctrl-F`): all non-overlapping byte ranges of `needle`
/// in `haystack`, in document order, applying **smartcase** (case-insensitive unless `needle`
/// contains an uppercase letter).  Every offset is a char boundary, so the ranges are UTF-8-safe.
///
/// Smartcase lives here, in the base search feature, so the `Ctrl-F` flow and vim's `/` share it.
/// The replace flow uses [`find_all_cs`] so a lowercase term never overwrites a casing variant.
pub fn find_all(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    if needle.chars().any(char::is_uppercase) {
        return find_all_cs(haystack, needle);
    }
    find_all_ci(haystack, needle)
}

/// Case-sensitive, non-overlapping match search — the matcher the replace flow always uses.
/// `match_indices` is non-overlapping by construction and yields only char-boundary offsets.
fn find_all_cs(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    haystack
        .match_indices(needle)
        .map(|(start, m)| start..start + m.len())
        .collect()
}

/// Case-insensitive, non-overlapping match search.  Compares char-by-char against the untouched
/// source: lowercasing up front would shift byte offsets for chars whose lowercase form differs in
/// byte length.
fn find_all_ci(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    let needle_chars: Vec<char> = needle.chars().collect();
    let hay_chars: Vec<(usize, char)> = haystack.char_indices().collect();
    let n = needle_chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i + n <= hay_chars.len() {
        let matched = (0..n).all(|k| chars_eq_ci(hay_chars[i + k].1, needle_chars[k]));
        if matched {
            let start = hay_chars[i].0;
            let end = hay_chars
                .get(i + n)
                .map_or(haystack.len(), |&(byte, _)| byte);
            out.push(start..end);
            i += n; // non-overlapping, mirroring `match_indices`
        } else {
            i += 1;
        }
    }
    out
}

/// Compare two chars ignoring case, via `to_lowercase` (simple case folding).
fn chars_eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_all_locates_every_occurrence_in_order() {
        assert_eq!(find_all("abcabcabc", "abc"), vec![0..3, 3..6, 6..9]);
        assert_eq!(find_all("no hits here", "xyz"), Vec::<Range<usize>>::new());
    }

    #[test]
    fn find_all_is_non_overlapping() {
        // Both the case-sensitive and case-insensitive paths must skip the overlap at 1.
        assert_eq!(find_all("aaa", "aa"), vec![0..2]);
        assert_eq!(find_all("AAA", "aa"), vec![0..2]);
    }

    #[test]
    fn find_all_smartcase_lowercase_query_is_insensitive() {
        assert_eq!(find_all("Foo foo FOO", "foo"), vec![0..3, 4..7, 8..11]);
    }

    #[test]
    fn find_all_smartcase_uppercase_query_is_sensitive() {
        assert_eq!(find_all("Foo foo FOO", "Foo"), vec![0..3]);
        assert_eq!(find_all("Foo foo FOO", "FOO"), vec![8..11]);
    }

    #[test]
    fn find_all_ci_keeps_byte_offsets_aligned_for_multibyte() {
        let hay = "café CAFÉ";
        let ranges = find_all(hay, "café");
        assert_eq!(ranges.len(), 2);
        // Slicing panics if an offset landed off a char boundary.
        let hits: Vec<&str> = ranges.iter().map(|r| &hay[r.clone()]).collect();
        assert_eq!(hits, vec!["café", "CAFÉ"]);
    }

    #[test]
    fn find_all_handles_multibyte_needles_and_haystacks() {
        let hay = "naïve café naïve";
        let ranges = find_all(hay, "naïve");
        assert_eq!(ranges.len(), 2);
        for r in ranges {
            assert_eq!(&hay[r], "naïve");
        }
    }

    #[test]
    fn replace_flow_matching_is_case_sensitive_not_smartcase() {
        let mut nav = SearchState::new("foo".to_owned(), None).unwrap();
        nav.ensure_fresh("Foo foo FOO", 1);
        assert_eq!(nav.matches.len(), 3);
        // The same query in a replace flow stays case-sensitive.
        let mut repl = SearchState::new("foo".to_owned(), Some("bar".to_owned())).unwrap();
        repl.ensure_fresh("Foo foo FOO", 1);
        assert_eq!(repl.matches, vec![4..7]);
    }

    #[test]
    fn new_rejects_an_empty_query_and_a_bad_escape() {
        assert_eq!(
            SearchState::new(String::new(), None),
            Err(SearchError::Empty)
        );
        assert_eq!(
            SearchState::new(r"\d".to_owned(), None),
            Err(SearchError::Escape(EscapeError::Unsupported('d')))
        );
        // A bad escape in the *replace* field is caught too.
        assert_eq!(
            SearchState::new("ok".to_owned(), Some(r"a\".to_owned())),
            Err(SearchError::Escape(EscapeError::Trailing))
        );
        assert!(SearchState::new("ok".to_owned(), None).is_ok());
    }

    #[test]
    fn new_decodes_escapes_into_the_needle_and_keeps_the_typed_query() {
        let s = SearchState::new(r"  \n".to_owned(), Some(r"\t".to_owned())).unwrap();
        assert_eq!(s.query, r"  \n");
        assert_eq!(s.needle, "  \n");
        assert_eq!(s.replacement.as_deref(), Some("\t"));
    }

    #[test]
    fn a_multiline_needle_matches_across_a_line_break() {
        let mut s = SearchState::new(r"  \n".to_owned(), None).unwrap();
        s.ensure_fresh("foo  \nbar  \nbaz", 1);
        assert_eq!(s.matches, vec![3..6, 9..12]);
    }

    fn fresh(source: &str, query: &str) -> SearchState {
        let mut s = SearchState::new(query.to_owned(), None).unwrap();
        s.ensure_fresh(source, 1);
        s
    }

    #[test]
    fn ensure_fresh_skips_when_version_unchanged() {
        let mut s = fresh("aba", "a");
        assert_eq!(s.matches.len(), 2);
        // Same version: no recompute even though the source text differs.
        assert!(!s.ensure_fresh("bbb", 1));
        assert_eq!(s.matches.len(), 2);
        assert!(s.ensure_fresh("bbb", 2));
        assert!(s.matches.is_empty());
    }

    #[test]
    fn focus_relative_to_wraps_around_the_document() {
        // Matches at 0, 3, 6.
        let mut s = fresh("abcabcabc", "abc");
        s.focus_relative_to(0, true);
        assert_eq!(s.focused_idx, 1, "first match strictly after the cursor");
        s.focus_relative_to(6, true);
        assert_eq!(s.focused_idx, 0, "forward wraps past the last match");
        s.focus_relative_to(6, false);
        assert_eq!(s.focused_idx, 1, "last match strictly before the cursor");
        s.focus_relative_to(0, false);
        assert_eq!(s.focused_idx, 2, "backward wraps past the first match");
    }

    #[test]
    fn navigation_wraps_both_directions() {
        let mut s = fresh("x.x.x", "x");
        assert_eq!(s.focused_idx, 0);
        s.advance_focus();
        s.advance_focus();
        assert_eq!(s.focused_idx, 2);
        s.advance_focus();
        assert_eq!(s.focused_idx, 0, "next past last wraps to first");
        s.retreat_focus();
        assert_eq!(s.focused_idx, 2, "prev before first wraps to last");
    }

    #[test]
    fn ensure_fresh_wraps_out_of_range_focus_to_first() {
        let mut s = fresh("x x x", "x");
        s.focused_idx = 2;
        s.ensure_fresh("x", 2);
        assert_eq!(s.matches.len(), 1);
        assert_eq!(s.focused_idx, 0);
    }
}
