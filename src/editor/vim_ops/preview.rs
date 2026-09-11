//! Live `:s` substitution preview — neovim's `inccommand=nosplit`.  See
//! `docs/dev/substitute-preview.md`.
//!
//! Every keystroke goes through [`update_substitute_preview`], which **reverts** the previous
//! preview and recomputes against the pristine buffer — never diffing two previews.  Enter reverts
//! first too, so the real [`execute_substitute`](super::ex::execute_substitute) sees an untouched
//! buffer and its undo / flash semantics match a preview-less submit.
//!
//! Preview edits use the raw [`Buffer`] primitives, so no undo delta is recorded and `dirty` is
//! untouched.  The stashed inverse delta is stamped with the `Buffer::version()` it was applied at;
//! a revert on a mismatched version drops the preview rather than corrupting text.

use fancy_regex::RegexBuilder;

use crate::document::{Buffer, EditDelta};
use crate::editor::vim_ops::ex::{
    build_substitution, for_each_region_match, parse_ex, region_haystack, resolve_substitute_lines,
    ExCommand, ExError, Substitution,
};
use crate::editor::vim_ops::vim_regex::translate_pattern;
use crate::editor::EditorState;

/// Matches past this count are left untouched; the scanned lines are still rewritten correctly.
const MAX_PREVIEW_MATCHES: usize = 1_000;

/// `fancy-regex` backtrack limit for the preview only (the commit path keeps the crate default):
/// a pathological half-typed pattern like `(a+)+b` must fail fast, not hang the UI.
const BACKTRACK_LIMIT: usize = 100_000;

/// Live `:s` preview state, held on [`EditorState`] so the overlay painters can read it.
pub struct SubstitutePreview {
    /// Byte ranges to highlight, valid against the CURRENT (possibly preview-modified) buffer:
    /// match ranges while the replacement field is absent, inserted-segment ranges once it is
    /// present.  Sorted, non-overlapping.
    pub highlights: Vec<std::ops::Range<usize>>,
    /// Inverse delta restoring the original text.  `None` for a highlight-only preview.
    revert: Option<EditDelta>,
    /// `Buffer::version()` just after the preview edit.  A mismatch means a mutation slipped past
    /// the gates and the original text is gone, so the revert is refused.
    applied_version: u64,
    /// Cursor offset at session start, restored on cancel and before submit (so `:s`'s
    /// current-line resolution sees the original cursor).
    saved_cursor: usize,
    /// Viewport scroll at session start, restored on cancel only.
    saved_scroll: usize,
}

/// One preview frame, computed against an unmodified buffer.  Pure data — the test surface.
pub struct PreviewPlan {
    /// Combined substitution delta (char offsets).  `None` for a highlight-only preview.
    pub delta: Option<EditDelta>,
    /// Highlight byte ranges: pre-apply match ranges when `delta` is `None`, post-apply
    /// inserted-segment ranges otherwise (zero-width deletion segments filtered out).
    pub highlights: Vec<std::ops::Range<usize>>,
    /// First buffer line that matched, for scroll-into-view.
    pub first_line: usize,
}

// ── Compute ─────────────────────────────────────────────────────────────────

/// Compute the preview for one substitution against the pristine buffer.  `Ok(None)` means
/// "nothing to preview".  Regex errors surface as `Err`; the caller treats them like `Ok(None)`
/// (a half-typed pattern must never flash an error), but tests can tell them apart.
pub fn compute_preview_plan(
    buffer: &Buffer,
    cursor_line: usize,
    sub: &Substitution,
    visual_range: Option<(usize, usize)>,
) -> Result<Option<PreviewPlan>, ExError> {
    if sub.pattern.is_empty() {
        return Ok(None);
    }
    let translated = translate_pattern(&sub.pattern)?;
    // `multi_line` must match the commit path's builder, or `^`/`$` would anchor differently
    // from what Enter commits.
    let re = RegexBuilder::new(&translated)
        .case_insensitive(sub.ignore_case)
        .multi_line(true)
        .backtrack_limit(BACKTRACK_LIMIT)
        .build()
        .map_err(|e| ExError::InvalidRegex(e.to_string()))?;

    if sub.replacement_present {
        let Some(edit) = build_substitution(
            buffer,
            cursor_line,
            &re,
            sub,
            visual_range,
            Some(MAX_PREVIEW_MATCHES),
        )?
        else {
            return Ok(None);
        };
        return Ok(Some(PreviewPlan {
            delta: Some(edit.delta),
            // A deletion preview inserts nothing, so there is no cell to highlight.
            highlights: edit
                .replaced_ranges
                .into_iter()
                .filter(|r| r.start < r.end)
                .collect(),
            first_line: edit.first_match_line,
        }));
    }

    // Highlight-only: collect the ranges the substitution *would* touch.  Driving the same walker
    // as the replacement path is what makes that exact — one implementation of the
    // first-match-per-line rule and the region bound, so highlights can't disagree with Enter.
    let Some((first, last)) =
        resolve_substitute_lines(buffer, cursor_line, sub.range, visual_range)
    else {
        return Ok(None);
    };
    let (hay, _start_char, base_byte) = region_haystack(buffer, first, last);
    let mut highlights: Vec<std::ops::Range<usize>> = Vec::new();
    let mut first_match_line = None;
    for_each_region_match(buffer, base_byte, &hay, &re, sub.global, |caps, line| {
        let m = caps.get(0).expect("group 0 is always present");
        first_match_line.get_or_insert(line);
        if m.start() < m.end() {
            highlights.push(base_byte + m.start()..base_byte + m.end());
        }
        if highlights.len() >= MAX_PREVIEW_MATCHES {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    })?;
    match first_match_line {
        Some(line) => Ok(Some(PreviewPlan {
            delta: None,
            highlights,
            first_line: line,
        })),
        None => Ok(None),
    }
}

// ── Apply / revert ──────────────────────────────────────────────────────────

/// Re-derive the preview from the current command-line text, reverting any existing preview first
/// so the plan is computed against the pristine buffer.  Any parse / regex error, non-substitute
/// command, or matchless pattern silently ends the session — no error spam mid-typing.
pub fn update_substitute_preview(
    editor: &mut EditorState,
    input: &str,
    visual_range: Option<(usize, usize)>,
    viewport_height: usize,
    viewport_width: usize,
) {
    let (saved_cursor, saved_scroll, had_prior) = match editor.substitute_preview.take() {
        Some(prior) => {
            let saved = (prior.saved_cursor, prior.saved_scroll);
            if !apply_revert(editor, prior) {
                // Revert refused (version mismatch): end the session outright, since a "fresh"
                // plan would stack edits on the orphaned preview text and stash a revert to it.
                return;
            }
            (saved.0, saved.1, true)
        }
        None => (editor.cursor.offset, editor.scroll, false),
    };

    let plan = match parse_ex(input) {
        Ok(ExCommand::Substitute(sub)) => {
            let cursor_line = editor
                .buffer
                .char_to_line(saved_cursor.min(editor.buffer.len_chars()));
            compute_preview_plan(&editor.buffer, cursor_line, &sub, visual_range)
                .ok()
                .flatten()
        }
        _ => None,
    };
    let Some(plan) = plan else {
        if had_prior {
            editor.restore_view(saved_cursor, Some(saved_scroll));
        }
        return;
    };

    let (revert, applied_version) = match plan.delta {
        Some(delta) => {
            apply_raw(editor, &delta);
            (
                Some(EditDelta {
                    offset: delta.offset,
                    removed: delta.inserted,
                    inserted: delta.removed,
                }),
                editor.buffer.version(),
            )
        }
        None => (None, editor.buffer.version()),
    };

    // Park the cursor at the first affected line, as nvim's inccommand does.  Every recompute and
    // the session end use `saved_cursor`, so the park never leaks into semantics.  `first_line`
    // still names the same text post-apply (everything before the first match is byte-identical);
    // the `min` only guards a preview that consumed the tail of the buffer.
    let target = editor.buffer.line_to_char(
        plan.first_line
            .min(editor.buffer.line_count().saturating_sub(1)),
    );
    editor.place_cursor(target);
    editor.scroll_cursor_comfortably_into_view(viewport_height, viewport_width);

    editor.substitute_preview = Some(SubstitutePreview {
        highlights: plan.highlights,
        revert,
        applied_version,
        saved_cursor,
        saved_scroll,
    });
}

/// Revert and drop the preview, returning whether a session existed.  The cursor always returns to
/// its pre-preview offset (submit's current-line resolution needs it); `restore_view` additionally
/// restores the scroll — false on submit, where `execute_substitute` places the view.  A refused
/// revert restores neither: the buffer holds foreign text the saved positions don't belong to.
pub fn clear_substitute_preview(editor: &mut EditorState, restore_view: bool) -> bool {
    let Some(preview) = editor.substitute_preview.take() else {
        return false;
    };
    let saved_cursor = preview.saved_cursor;
    let saved_scroll = preview.saved_scroll;
    if apply_revert(editor, preview) {
        editor.restore_view(saved_cursor, restore_view.then_some(saved_scroll));
    }
    true
}

/// Apply the preview's inverse delta through the raw buffer primitives.  Returns `false` when a
/// version mismatch refuses the revert: the stashed original no longer lines up, so dropping the
/// preview beats corrupting text.  A highlight-only preview has nothing to undo and succeeds.
fn apply_revert(editor: &mut EditorState, preview: SubstitutePreview) -> bool {
    let Some(revert) = preview.revert else {
        return true;
    };
    if editor.buffer.version() != preview.applied_version {
        return false;
    }
    apply_raw(editor, &revert);
    true
}

/// Apply `delta` via the raw [`Buffer`] primitives — no undo delta, `dirty` untouched — then
/// re-parse so the next frame renders the new text.
fn apply_raw(editor: &mut EditorState, delta: &EditDelta) {
    if !delta.removed.is_empty() {
        let end = delta.offset + delta.removed.chars().count();
        editor
            .buffer
            .remove(delta.offset, end.min(editor.buffer.len_chars()));
    }
    if !delta.inserted.is_empty() {
        editor.buffer.insert(delta.offset, &delta.inserted);
    }
    editor.refresh_parsed();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::editor::vim_ops::ex::SubstituteRange;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn editor(text: &str) -> EditorState {
        let mut st = EditorState::new(Buffer::from_str(text), theme());
        st.update_cursor_block();
        st
    }

    /// `rep: None` = replacement field absent (`:s/pat`, highlight-only);
    /// `Some` = present (`:s/pat/rep`, inline preview).
    fn sub(range: SubstituteRange, pat: &str, rep: Option<&str>, global: bool) -> Substitution {
        Substitution {
            range,
            pattern: pat.to_owned(),
            replacement: rep.unwrap_or("").to_owned(),
            replacement_present: rep.is_some(),
            global,
            ignore_case: false,
        }
    }

    // ── compute_preview_plan ────────────────────────────────────────

    #[test]
    fn highlight_only_collects_match_ranges_without_a_delta() {
        let buf = Buffer::from_str("foo bar\nbaz foo\n");
        let plan = compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "foo", None, false),
            None,
        )
        .unwrap()
        .expect("matches exist");
        assert!(plan.delta.is_none(), "no replacement field → no edit");
        // First match per line (no `g` while typing the pattern).
        assert_eq!(plan.highlights, vec![0..3, 12..15]);
        assert_eq!(plan.first_line, 0);
    }

    #[test]
    fn replacement_plan_carries_post_apply_inserted_ranges() {
        let buf = Buffer::from_str("foo foo\nfoo");
        let plan = compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "foo", Some("xy"), true),
            None,
        )
        .unwrap()
        .expect("matches exist");
        let delta = plan.delta.expect("replacement present → delta");
        assert_eq!(delta.offset, 0);
        assert_eq!(delta.removed, "foo foo\nfoo");
        assert_eq!(delta.inserted, "xy xy\nxy");
        assert_eq!(plan.highlights, vec![0..2, 3..5, 6..8]);
    }

    #[test]
    fn deletion_preview_keeps_the_delta_but_drops_zero_width_highlights() {
        let buf = Buffer::from_str("a foo b");
        let plan = compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "foo ", Some(""), false),
            None,
        )
        .unwrap()
        .expect("matches exist");
        assert_eq!(plan.delta.expect("deletion edits").inserted, "a b");
        assert!(
            plan.highlights.is_empty(),
            "zero-width inserted segments have no cell to paint"
        );
    }

    #[test]
    fn multibyte_highlights_stay_on_char_boundaries() {
        let buf = Buffer::from_str("héllo héllo");
        let plan = compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "héllo", None, true),
            None,
        )
        .unwrap()
        .expect("matches exist");
        assert_eq!(plan.highlights, vec![0..6, 7..13]);
        let src = buf.contents();
        for r in &plan.highlights {
            assert!(src.is_char_boundary(r.start) && src.is_char_boundary(r.end));
        }
    }

    #[test]
    fn empty_pattern_and_no_match_produce_no_plan() {
        let buf = Buffer::from_str("abc");
        assert!(compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "", None, false),
            None
        )
        .unwrap()
        .is_none());
        assert!(compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "zzz", Some("x"), false),
            None
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn invalid_regex_surfaces_as_err() {
        let buf = Buffer::from_str("abc");
        assert!(compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "a\\v(", None, false),
            None
        )
        .is_err());
    }

    #[test]
    fn current_line_and_visual_range_scope_the_scan() {
        let buf = Buffer::from_str("foo\nfoo\nfoo");
        let plan = compute_preview_plan(
            &buf,
            1,
            &sub(SubstituteRange::CurrentLine, "foo", None, false),
            None,
        )
        .unwrap()
        .expect("current line matches");
        assert_eq!(plan.highlights, vec![4..7]);
        let plan = compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::VisualRange, "foo", None, false),
            Some((1, 2)),
        )
        .unwrap()
        .expect("visual range matches");
        assert_eq!(plan.highlights, vec![4..7, 8..11]);
    }

    // ── update / clear round-trip ───────────────────────────────────

    #[test]
    fn update_previews_the_replacement_without_history_or_dirty() {
        let mut st = editor("foo bar\nfoo");
        update_substitute_preview(&mut st, "%s/foo/quux/g", None, 24, 80);
        assert_eq!(st.buffer.contents(), "quux bar\nquux");
        assert!(!st.dirty, "preview must not dirty the buffer");
        let preview = st.substitute_preview.as_ref().expect("preview active");
        assert_eq!(preview.highlights, vec![0..4, 9..13]);
        assert_eq!(
            st.history.undo_depth(),
            0,
            "no undo delta may be recorded for a preview"
        );
    }

    #[test]
    fn every_keystroke_recomputes_against_the_pristine_buffer() {
        let mut st = editor("foo");
        update_substitute_preview(&mut st, "%s/foo/ba", None, 24, 80);
        assert_eq!(st.buffer.contents(), "ba");
        // Derived from the ORIGINAL text, not from the previewed "ba".
        update_substitute_preview(&mut st, "%s/foo/bar", None, 24, 80);
        assert_eq!(st.buffer.contents(), "bar");
        // Backspacing past the second delimiter: deletion preview, then highlight-only.
        update_substitute_preview(&mut st, "%s/foo/", None, 24, 80);
        assert_eq!(st.buffer.contents(), "", "deletion preview");
        update_substitute_preview(&mut st, "%s/foo", None, 24, 80);
        assert_eq!(st.buffer.contents(), "foo");
        let preview = st.substitute_preview.as_ref().expect("highlight-only");
        assert_eq!(preview.highlights, vec![0..3]);
    }

    #[test]
    fn clear_restores_text_cursor_and_scroll() {
        let mut st = editor("one\n\ntwo\n\nfoo");
        st.cursor.offset = 2;
        st.scroll = 1;
        update_substitute_preview(&mut st, "%s/foo/bar/", None, 2, 80);
        assert_eq!(st.buffer.contents(), "one\n\ntwo\n\nbar");
        assert!(clear_substitute_preview(&mut st, /*restore_view=*/ true));
        assert_eq!(st.buffer.contents(), "one\n\ntwo\n\nfoo");
        assert_eq!(st.cursor.offset, 2, "cursor restored");
        assert_eq!(st.scroll, 1, "scroll restored on cancel");
        assert!(st.substitute_preview.is_none());
        assert!(!clear_substitute_preview(&mut st, true), "already cleared");
    }

    #[test]
    fn a_non_substitute_line_ends_the_session_and_restores_the_view() {
        let mut st = editor("foo");
        update_substitute_preview(&mut st, "%s/foo/bar/", None, 24, 80);
        assert_eq!(st.buffer.contents(), "bar");
        // Backspaced down to `:w` — not a substitution.
        update_substitute_preview(&mut st, "w", None, 24, 80);
        assert_eq!(st.buffer.contents(), "foo", "preview reverted");
        assert!(st.substitute_preview.is_none());
    }

    #[test]
    fn version_mismatch_drops_the_preview_without_touching_the_buffer() {
        let mut st = editor("foo");
        update_substitute_preview(&mut st, "%s/foo/bar/", None, 24, 80);
        assert_eq!(st.buffer.contents(), "bar");
        // Nothing should allow this mutation; the version stamp is the fail-safe.
        st.buffer.insert(0, "X");
        clear_substitute_preview(&mut st, true);
        assert_eq!(
            st.buffer.contents(),
            "Xbar",
            "a mismatched revert must be refused, not misapplied"
        );
        assert!(st.substitute_preview.is_none());
    }

    #[test]
    fn update_after_a_gate_slip_ends_the_session_instead_of_compounding() {
        let mut st = editor("foo");
        update_substitute_preview(&mut st, "%s/foo/bar/", None, 24, 80);
        assert_eq!(st.buffer.contents(), "bar");
        // Nothing should allow this mutation; the version stamp is the fail-safe.
        st.buffer.insert(0, "X");
        update_substitute_preview(&mut st, "%s/bar/QQ/", None, 24, 80);
        assert_eq!(
            st.buffer.contents(),
            "Xbar",
            "no new preview may apply on top of orphaned preview text"
        );
        assert!(st.substitute_preview.is_none());
    }
}
