//! `DiffState` — the in-flight review session attached to `EditorState::diff` while `Mode::Diff`
//! is active.  Owns the pre-change rope, the working new-side buffer, the hunk list, per-hunk
//! decisions, and focus.  See `docs/dev/diff-review.md`.
//!
//! Hunk decisions are deliberately not undoable (recover by re-deciding or `DiffResetHunk`; a bulk
//! flip is guarded by a confirmation modal), so `Action::Undo` / `Action::Redo` are no-ops here.

use std::cell::{Cell, RefCell};

use ropey::Rope;

use crate::document::{Buffer, Cursor, ParsedDoc};

use super::engine::{
    compute, hunk_new_side_text, match_by_old_overlap, pending_decisions, HunkIdAllocator,
};
use super::hunk::{Decision, Hunk, HunkId};
use super::layout::DiffLayoutCache;

/// Outcome of [`DiffState::reconcile_with_disk`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// Hunks remain.  `reset` counts hunks dropped back to `Pending` because their new-side
    /// target changed; it drives the flash wording.
    StillReviewing { reset: usize },
    /// Every change was reverted, so the caller exits diff mode.
    NoChangesRemain,
}

/// Owned by `EditorState::diff` for the duration of a review; see `EditorState::enter_diff_mode`.
pub struct DiffState {
    /// The in-memory buffer at the moment the watcher reported a disk change.  Immutable across
    /// the review — every recomputation runs against it and the current `new_buffer`.
    pub old_rope: Rope,
    /// Working copy of the new-side text, seeded from disk at diff entry.  Read-only today; the
    /// buffer wrapper exists so a future Edit mode can route diff-side writes through it.
    pub new_buffer: Buffer,
    /// Cursor into `new_buffer.rope()`, reserved for a future in-diff Edit mode.
    #[allow(dead_code)]
    pub cursor: Cursor,
    /// Per-hunk diff list, ordered by document position.
    pub hunks: Vec<Hunk>,
    /// Decision per hunk; `decisions[i]` corresponds to `hunks[i]`.
    pub decisions: Vec<Decision>,
    /// Focused hunk.  An id rather than an index so it survives the shuffle a recompute causes.
    pub focused_id: HunkId,
    /// Monotonic id allocator, kept here so a recompute can mint ids that don't collide with
    /// previously-issued ones.
    pub(crate) ids: HunkIdAllocator,
    /// True when a table's uneven cell counts forced the coarser line-level hunks instead of
    /// per-row ones.  `App::enter_diff_mode` flashes a hint so the user knows why.
    pub uneven_table_fallback: bool,
    /// `true` for the read-only difftool presentation (`edamame --diff <old> <new>`).
    ///
    /// git picks both paths, usually including a temp file it deletes on our exit, so a decision
    /// would be written somewhere the user never named or silently discarded.  The flag therefore
    /// removes the whole decision vocabulary: the dispatcher denies the decision actions, the hint
    /// row advertises only navigation and the exits, the dividers drop their checkbox and prompt,
    /// and `Esc` leaves the process.  All four read this one flag — a surface still offering a
    /// decision the dispatcher refuses is the failure this shape prevents.
    pub read_only: bool,
    /// Rendered parse of the *new side*, when unchanged regions are shown as rendered Markdown.
    ///
    /// `None` means "render every line raw" — still correct, and the state every review starts and
    /// is briefly queried in.  Nothing stamps the parse against the buffer it came from, so
    /// [`Self::reconcile_with_disk`] drops it when it replaces `new_buffer`.  It is rebuilt by
    /// [`crate::editor::EditorState::refresh_diff_parse`]: `DiffState` holds no theme or width, so
    /// deciding *when* to rebuild stays with the state that tracks both.
    pub parsed_new: Option<ParsedDoc>,
    /// Lazily-built visual-line list + row-count cache (see [`super::layout`]).  Interior-mutable
    /// so the immutable render / scroll-query paths can populate it on first use.
    pub(crate) layout: RefCell<DiffLayoutCache>,
    /// The diff's analogue of `ParsedDoc::parsed_version`, and the cache key for the diff-side
    /// image-snapshot geometry.  A `Cell` because every line-set mutation must bump it and
    /// `invalidate_layout` takes `&self`.
    layout_version: Cell<u64>,
}

impl DiffState {
    /// Build a diff state from the pre-change buffer text and the just-read disk contents.
    /// `None` when they are byte-equal: an empty hunk list defeats the `focused_id = hunks[0].id`
    /// invariant, and the caller already short-circuits that case.
    pub fn new(old: &str, new: &str) -> Option<Self> {
        let mut ids = HunkIdAllocator::new();
        let computation = compute(old, new, &mut ids);
        let hunks = computation.hunks;
        if hunks.is_empty() {
            return None;
        }
        let decisions = pending_decisions(&hunks);
        let focused_id = hunks[0].id;
        let old_rope = Rope::from_str(old);
        let new_rope = Rope::from_str(new);
        let new_buffer = Buffer::from_rope(new_rope);
        // The focused hunk's new-side first line — the canonical anchor even for a Delete hunk,
        // whose new-side range is empty.
        let cursor_offset = new_buffer
            .rope()
            .line_to_char(hunks[0].new_lines.start.min(new_buffer.line_count()));
        let mut cursor = Cursor::new();
        cursor.offset = cursor_offset;
        Some(Self {
            old_rope,
            new_buffer,
            cursor,
            hunks,
            decisions,
            focused_id,
            ids,
            uneven_table_fallback: computation.uneven_table_fallback,
            read_only: false,
            parsed_new: None,
            layout: RefCell::new(DiffLayoutCache::default()),
            layout_version: Cell::new(0),
        })
    }

    /// Install (or drop) the rendered new-side parse, always invalidating the layout: the
    /// visual-line *set* depends on the parse, not just the hunk list.  The `Option` keeps the raw
    /// state expressible, which [`Self::reconcile_with_disk`] relies on.
    pub fn set_rendered_parse(&mut self, parsed: Option<ParsedDoc>) {
        self.parsed_new = parsed;
        self.invalidate_layout();
    }

    /// Current layout version — see [`Self::layout_version`].
    pub(crate) fn layout_version(&self) -> u64 {
        self.layout_version.get()
    }

    /// Advance the layout version.  Called only from [`Self::invalidate_layout`], the single
    /// funnel every line-set change goes through.
    pub(crate) fn bump_layout_version(&self) {
        self.layout_version
            .set(self.layout_version.get().wrapping_add(1));
    }

    /// Index of the focused hunk, or `None` when a recompute dropped it.
    pub fn focused_idx(&self) -> Option<usize> {
        self.hunks.iter().position(|h| h.id == self.focused_id)
    }

    /// First still-`Pending` hunk in document order — the focus fallback after a recompute.
    pub fn first_pending_id(&self) -> Option<HunkId> {
        self.hunks
            .iter()
            .zip(self.decisions.iter())
            .find(|(_, d)| **d == Decision::Pending)
            .map(|(h, _)| h.id)
    }

    /// Fold a fresh on-disk write into the live review **in place**, preserving every decision the
    /// user made on hunks the write did not touch.
    ///
    /// `old_rope` is invariant, so the diff is recomputed against the new disk contents and each
    /// new hunk matched to a prior one by old-side overlap ([`match_by_old_overlap`]).  A match
    /// carries its decision forward **only when its new-side text is byte-identical** to what was
    /// reviewed; otherwise it resets to `Pending`, because the user never saw this change.  Hunks
    /// that no longer exist drop their decisions.
    ///
    /// **`NoChangesRemain` leaves `self` half-cleared and invalid** (`hunks` / `decisions` taken
    /// and not restored).  The only valid response is to exit diff mode.
    pub fn reconcile_with_disk(&mut self, new_disk: &str) -> ReconcileOutcome {
        let prior_hunks = std::mem::take(&mut self.hunks);
        let prior_decisions = std::mem::take(&mut self.decisions);
        let prior_new_rope = self.new_buffer.rope().clone();
        let prior_focused = self.focused_id;

        let old = self.old_rope.to_string();
        let computation = compute(&old, new_disk, &mut self.ids);
        let mut hunks = computation.hunks;
        if hunks.is_empty() {
            return ReconcileOutcome::NoChangesRemain;
        }
        let new_rope = Rope::from_str(new_disk);

        let mut decisions = vec![Decision::Pending; hunks.len()];
        let mut reset = 0usize;
        // A prior hunk may be adopted by at most one new hunk: a prior spanning several old lines
        // can split into two new hunks that both overlap it, and without this guard both would
        // inherit its id.  The earliest claimant wins; later ones keep their fresh id and Pending.
        let mut claimed = vec![false; prior_hunks.len()];
        for (i, h) in hunks.iter_mut().enumerate() {
            let Some(p) = match_by_old_overlap(h, &prior_hunks) else {
                continue;
            };
            if claimed[p] {
                continue; // prior already adopted → keep this hunk's fresh id, Pending
            }
            claimed[p] = true;
            h.id = prior_hunks[p].id; // inherit the stable id
            let same_new = hunk_new_side_text(h, &new_rope)
                == hunk_new_side_text(&prior_hunks[p], &prior_new_rope);
            if same_new {
                decisions[i] = prior_decisions[p]; // carry the decision
            } else if prior_decisions[p] != Decision::Pending {
                reset += 1; // changed target → re-review
            }
        }

        self.new_buffer.set_rope(new_rope);
        self.hunks = hunks;
        self.decisions = decisions;
        self.uneven_table_fallback = computation.uneven_table_fallback;

        self.focused_id = if self.hunks.iter().any(|h| h.id == prior_focused) {
            prior_focused
        } else {
            self.first_pending_id().unwrap_or(self.hunks[0].id)
        };

        // Drop the rendered parse with the layout: it was built from the `new_buffer` just
        // replaced, and `build_visual_lines_rendered` reads source lines out of the parse while
        // taking the line count from the buffer.  `DiffState` can't rebuild it (no theme, no
        // width), so falling back to the raw layout until the caller's `refresh_diff_parse`
        // reinstalls one costs a frame of rendered presentation, never a stale line range.
        self.set_rendered_parse(None);
        ReconcileOutcome::StillReviewing { reset }
    }

    /// Set the focused hunk's decision *without* moving focus — the caller advances after a brief
    /// delay so the user sees the checkbox land first.  Returns `true` when applied.
    pub fn decide_focused(&mut self, decision: Decision) -> bool {
        let Some(idx) = self.focused_idx() else {
            return false;
        };
        self.decisions[idx] = decision;
        true
    }

    /// Reset the focused hunk to `Pending`, reporting whether anything was actually cleared.
    pub fn reset_focused(&mut self) -> bool {
        let Some(idx) = self.focused_idx() else {
            return false;
        };
        if self.decisions[idx] == Decision::Pending {
            return false;
        }
        self.decisions[idx] = Decision::Pending;
        true
    }

    /// The focused hunk's decision, if focus is still valid.
    pub fn focused_decision(&self) -> Option<Decision> {
        self.focused_idx().map(|idx| self.decisions[idx])
    }

    /// Move focus to the next still-`Pending` hunk, wrapping.  Returns `false` (leaving focus put)
    /// when nothing else is pending.
    pub fn advance_to_next_pending(&mut self) -> bool {
        let Some(idx) = self.focused_idx() else {
            return false;
        };
        match self.next_pending_after(idx) {
            Some(next) if next != idx => {
                self.focused_id = self.hunks[next].id;
                true
            }
            _ => false,
        }
    }

    /// Apply `decision` to *every* hunk, overriding prior choices.  Accept-all / reject-all are
    /// decisive whole-diff actions, so they also stay functional on an already-resolved diff,
    /// where a pending-only version would be a silent no-op.
    pub fn bulk_decide(&mut self, decision: Decision) {
        self.decisions.fill(decision);
    }

    /// Advance focus to the next hunk in document order, wrapping.  `true` when focus moved.
    pub fn advance_focus(&mut self) -> bool {
        let Some(idx) = self.focused_idx() else {
            return false;
        };
        if self.hunks.is_empty() {
            return false;
        }
        let next = (idx + 1) % self.hunks.len();
        if next == idx {
            return false;
        }
        self.focused_id = self.hunks[next].id;
        true
    }

    /// Retreat focus to the previous hunk in document order.
    pub fn retreat_focus(&mut self) -> bool {
        let Some(idx) = self.focused_idx() else {
            return false;
        };
        if self.hunks.is_empty() {
            return false;
        }
        let prev = if idx == 0 {
            self.hunks.len() - 1
        } else {
            idx - 1
        };
        if prev == idx {
            return false;
        }
        self.focused_id = self.hunks[prev].id;
        true
    }

    /// Number of hunks still awaiting a decision.
    pub fn pending_count(&self) -> usize {
        self.decisions
            .iter()
            .filter(|d| **d == Decision::Pending)
            .count()
    }

    /// Number of hunks accepted or rejected — the `resolved/total` status-bar counter.
    pub fn resolved_count(&self) -> usize {
        self.decisions
            .iter()
            .filter(|d| **d != Decision::Pending)
            .count()
    }

    /// True iff every hunk is decided.  Triggers the `DiffResolveConfirmModal`.
    pub fn all_resolved(&self) -> bool {
        self.pending_count() == 0
    }

    /// The merged rope: `Accepted` takes the new-side range, `Rejected` the old-side one.  `None`
    /// while any decision is still `Pending`, so the caller can flash rather than panic.
    pub fn resolved_rope(&self) -> Option<Rope> {
        if !self.all_resolved() {
            return None;
        }
        let old_text = self.old_rope.to_string();
        let new_text = self.new_buffer.contents();
        let old_rope = &self.old_rope;
        let new_rope = self.new_buffer.rope();
        let mut out = String::new();
        let mut new_cursor = 0usize; // line index into new_rope
        for (h, dec) in self.hunks.iter().zip(self.decisions.iter()) {
            // Unchanged context up to the hunk start, taken from `new_rope` (byte-identical on
            // both sides).
            let new_gap = h.new_lines.start.saturating_sub(new_cursor);
            for _ in 0..new_gap {
                if new_cursor < new_rope.len_lines() {
                    append_line(&mut out, &new_text, new_rope, new_cursor);
                    new_cursor += 1;
                }
            }
            match dec {
                Decision::Accepted => {
                    for i in h.new_lines.clone() {
                        if i < new_rope.len_lines() {
                            append_line(&mut out, &new_text, new_rope, i);
                        }
                    }
                }
                Decision::Rejected => {
                    for i in h.old_lines.clone() {
                        if i < old_rope.len_lines() {
                            append_line(&mut out, &old_text, old_rope, i);
                        }
                    }
                }
                Decision::Pending => unreachable!("guarded by all_resolved()"),
            }
            new_cursor = h.new_lines.end;
        }
        // Trailing context after the last hunk.
        while new_cursor < new_rope.len_lines() {
            append_line(&mut out, &new_text, new_rope, new_cursor);
            new_cursor += 1;
        }
        Some(Rope::from_str(&out))
    }

    fn next_pending_after(&self, start: usize) -> Option<usize> {
        if self.hunks.is_empty() {
            return None;
        }
        let n = self.hunks.len();
        for step in 1..=n {
            let i = (start + step) % n;
            if self.decisions[i] == Decision::Pending {
                return Some(i);
            }
        }
        // Nothing pending — leave focus put so manual navigation still works.
        Some(start)
    }
}

fn append_line(out: &mut String, text: &str, rope: &Rope, line_idx: usize) {
    let line_start = rope.line_to_byte(line_idx);
    let line_end = if line_idx + 1 < rope.len_lines() {
        rope.line_to_byte(line_idx + 1)
    } else {
        rope.len_bytes()
    };
    let raw = &text[line_start..line_end];
    out.push_str(raw);
    if !raw.ends_with('\n') && line_idx + 1 < rope.len_lines() {
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_diff_returns_none() {
        assert!(DiffState::new("same\n", "same\n").is_none());
    }

    #[test]
    fn accept_all_yields_new_rope() {
        let mut state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
        state.bulk_decide(Decision::Accepted);
        let resolved = state.resolved_rope().unwrap();
        assert_eq!(resolved.to_string(), "a\nB\nc\n");
    }

    #[test]
    fn reject_all_yields_old_rope() {
        let mut state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
        state.bulk_decide(Decision::Rejected);
        let resolved = state.resolved_rope().unwrap();
        assert_eq!(resolved.to_string(), "a\nb\nc\n");
    }

    #[test]
    fn mixed_decisions_pick_per_hunk() {
        // Two hunks: replace line 1, insert at end.
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\nD\n";
        let mut state = DiffState::new(old, new).unwrap();
        assert!(state.hunks.len() >= 2);
        state.decisions[0] = Decision::Accepted;
        state.decisions[1] = Decision::Rejected;
        let resolved = state.resolved_rope().unwrap();
        assert_eq!(resolved.to_string(), "a\nB\nc\n");
    }

    #[test]
    fn two_deletes_mixed_decisions_reconstruct_correctly() {
        // Regression: a Rejected Delete must not advance `new_cursor` past its zero-length
        // new-side range, or the gap before the next hunk skips or re-emits context lines.
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nc\ne\n";
        let mut state = DiffState::new(old, new).unwrap();
        assert_eq!(state.hunks.len(), 2);

        // Reject H1 (keep b), accept H2 (delete d).
        state.decisions[0] = Decision::Rejected;
        state.decisions[1] = Decision::Accepted;
        assert_eq!(state.resolved_rope().unwrap().to_string(), "a\nb\nc\ne\n");

        // Accept H1 (delete b), reject H2 (keep d).
        state.decisions[0] = Decision::Accepted;
        state.decisions[1] = Decision::Rejected;
        assert_eq!(state.resolved_rope().unwrap().to_string(), "a\nc\nd\ne\n");
    }

    #[test]
    fn all_rejected_yields_original_old_text() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nC\ne\n";
        let mut state = DiffState::new(old, new).unwrap();
        state.bulk_decide(Decision::Rejected);
        let resolved = state.resolved_rope().unwrap();
        assert_eq!(resolved.to_string(), old);
    }

    #[test]
    fn pending_count_tracks_decisions() {
        let mut state = DiffState::new("a\nb\n", "a\nB\n").unwrap();
        assert_eq!(state.pending_count(), 1);
        state.decide_focused(Decision::Accepted);
        assert_eq!(state.pending_count(), 0);
        assert!(state.all_resolved());
    }

    #[test]
    fn reset_focused_undecides_and_noops_when_pending() {
        let mut state = DiffState::new("a\nb\n", "a\nB\n").unwrap();
        assert_eq!(state.focused_decision(), Some(Decision::Pending));
        assert!(
            !state.reset_focused(),
            "resetting a Pending hunk is a no-op"
        );

        state.decide_focused(Decision::Accepted);
        assert_eq!(state.focused_decision(), Some(Decision::Accepted));
        assert!(state.reset_focused(), "resetting a decided hunk clears it");
        assert_eq!(state.focused_decision(), Some(Decision::Pending));
        assert_eq!(state.pending_count(), state.hunks.len());

        assert!(!state.reset_focused());
    }

    // ── Reconcile ──────────────────────────────────────────────────

    fn idx_of(state: &DiffState, id: HunkId) -> usize {
        state
            .hunks
            .iter()
            .position(|h| h.id == id)
            .expect("hunk id present")
    }

    /// A parse built from the replaced `new_buffer` must not survive: nothing pairs the two, and
    /// the rendered layout reads source lines from the parse but the line count from the buffer.
    #[test]
    fn reconcile_drops_the_stale_rendered_parse() {
        let theme: &'static crate::config::Theme =
            Box::leak(Box::new(crate::config::Theme::default()));
        let old = "# Title\n\nAlpha.\n";
        let new1 = "# Title\n\nALPHA.\n";
        let mut state = DiffState::new(old, new1).unwrap();
        state.set_rendered_parse(Some(ParsedDoc::build(new1, theme, true, 20)));
        assert!(state.parsed_new.is_some());

        let outcome = state.reconcile_with_disk("# Title\n\nALPHA!\n");
        assert_eq!(outcome, ReconcileOutcome::StillReviewing { reset: 0 });
        assert!(
            state.parsed_new.is_none(),
            "a parse built from the replaced buffer must not survive"
        );
        // And the layout it feeds is rebuilt raw rather than reused.
        let raw = DiffState::new(old, "# Title\n\nALPHA!\n").unwrap();
        assert_eq!(state.total_visual_rows(80), raw.total_visual_rows(80));
    }

    #[test]
    fn reconcile_preserves_decision_on_unchanged_hunk() {
        // Two Replace hunks separated by a context line; accept h0, reject h1.
        let old = "a\nb\nc\nd\ne\n";
        let new1 = "a\nB\nc\nD\ne\n";
        let mut state = DiffState::new(old, new1).unwrap();
        assert_eq!(state.hunks.len(), 2);
        let h0_id = state.hunks[0].id;
        let h1_id = state.hunks[1].id;
        state.decisions[0] = Decision::Accepted;
        state.decisions[1] = Decision::Rejected;

        // An external write touching only h1's region.
        let new2 = "a\nB\nc\nDD\ne\n";
        let outcome = state.reconcile_with_disk(new2);

        assert_eq!(outcome, ReconcileOutcome::StillReviewing { reset: 1 });
        let h0 = idx_of(&state, h0_id);
        assert_eq!(state.decisions[h0], Decision::Accepted);
        // h1 keeps its id, but its changed target reset it.
        let h1 = idx_of(&state, h1_id);
        assert_eq!(state.decisions[h1], Decision::Pending);
    }

    #[test]
    fn reconcile_resets_decision_when_new_side_changes() {
        let mut state = DiffState::new("a\nb\n", "a\nB\n").unwrap();
        state.decisions[0] = Decision::Accepted;
        let outcome = state.reconcile_with_disk("a\nC\n");
        assert_eq!(outcome, ReconcileOutcome::StillReviewing { reset: 1 });
        assert_eq!(state.decisions[0], Decision::Pending);
    }

    #[test]
    fn reconcile_keeps_ids_unique_when_a_prior_hunk_splits() {
        // One multi-line Replace hunk over old lines 1..4.
        let old = "a\nb\nc\nd\ne\n";
        let new1 = "a\nB\nC\nD\ne\n";
        let mut state = DiffState::new(old, new1).unwrap();
        assert_eq!(state.hunks.len(), 1, "single coalesced replace hunk");
        let prior_id = state.hunks[0].id;
        state.decisions[0] = Decision::Accepted;

        // Reverting the middle line splits the prior hunk into two, both overlapping its old
        // range, and neither matching the reviewed new-side text — so the decision resets.
        let outcome = state.reconcile_with_disk("a\nB\nc\nD\ne\n");
        assert_eq!(outcome, ReconcileOutcome::StillReviewing { reset: 1 });
        assert_eq!(state.hunks.len(), 2, "prior hunk split in two");

        // Without the claimed-prior guard both halves would inherit `prior_id`.
        assert_ne!(
            state.hunks[0].id, state.hunks[1].id,
            "split hunks must not share an id",
        );
        let inheritors = state.hunks.iter().filter(|h| h.id == prior_id).count();
        assert_eq!(inheritors, 1, "exactly one hunk inherits the prior id");
        assert_eq!(state.hunks[0].id, prior_id, "earliest claimant wins it");
        assert_eq!(state.decisions[0], Decision::Pending);
        assert_eq!(state.decisions[1], Decision::Pending);
    }

    #[test]
    fn reconcile_drops_vanished_hunk() {
        let old = "a\nb\nc\nd\ne\n";
        let new1 = "a\nB\nc\nD\ne\n";
        let mut state = DiffState::new(old, new1).unwrap();
        let h0_id = state.hunks[0].id;
        state.decisions[0] = Decision::Accepted;
        state.decisions[1] = Decision::Rejected;

        // Reverting h1's region leaves only h0; h1's decision is silently dropped.
        let new2 = "a\nB\nc\nd\ne\n";
        let outcome = state.reconcile_with_disk(new2);

        assert_eq!(outcome, ReconcileOutcome::StillReviewing { reset: 0 });
        assert_eq!(state.hunks.len(), 1);
        assert_eq!(state.hunks[0].id, h0_id);
        assert_eq!(state.decisions[0], Decision::Accepted);
    }

    #[test]
    fn reconcile_preserves_accepted_insertion() {
        // Regression: a pure Insert hunk (empty old-side range) must keep its decision across an
        // unrelated external write.
        let old = "a\nb\n";
        let new1 = "a\nNEW\nb\n";
        let mut state = DiffState::new(old, new1).unwrap();
        assert_eq!(state.hunks.len(), 1);
        assert_eq!(state.hunks[0].kind, crate::diff::HunkKind::Insert);
        let ins_id = state.hunks[0].id;
        state.decisions[0] = Decision::Accepted;

        let new2 = "a\nNEW\nb\nEXTRA\n";
        let outcome = state.reconcile_with_disk(new2);

        assert_eq!(outcome, ReconcileOutcome::StillReviewing { reset: 0 });
        assert_eq!(state.hunks.len(), 2);
        let ins = idx_of(&state, ins_id);
        assert_eq!(state.decisions[ins], Decision::Accepted);
        let other = 1 - ins;
        assert_eq!(state.decisions[other], Decision::Pending);
    }

    #[test]
    fn reconcile_collapses_to_no_changes() {
        let mut state = DiffState::new("a\nb\n", "a\nB\n").unwrap();
        // Disk reverted to the original buffer — nothing differs.
        let outcome = state.reconcile_with_disk("a\nb\n");
        assert_eq!(outcome, ReconcileOutcome::NoChangesRemain);
    }

    #[test]
    fn reconcile_focus_survives_or_falls_back() {
        let old = "a\nb\nc\nd\ne\n";
        let new1 = "a\nB\nc\nD\ne\n";

        // Focused hunk survives → focus kept.
        let mut state = DiffState::new(old, new1).unwrap();
        let h0_id = state.hunks[0].id;
        assert_eq!(state.focused_id, h0_id);
        state.reconcile_with_disk("a\nB\nc\nDD\ne\n");
        assert_eq!(state.focused_id, h0_id, "surviving focus is kept");

        // Focused hunk vanishes → focus lands on the first pending hunk.
        let mut state = DiffState::new(old, new1).unwrap();
        let h0_id = state.hunks[0].id;
        let h1_id = state.hunks[1].id;
        state.focused_id = h1_id;
        state.reconcile_with_disk("a\nB\nc\nd\ne\n");
        assert_eq!(
            state.focused_id, h0_id,
            "vanished focus falls back to the first pending hunk",
        );
    }

    #[test]
    fn resolved_count_climbs_as_hunks_are_decided() {
        let mut state = DiffState::new("a\nb\n", "a\nB\n").unwrap();
        assert_eq!(state.resolved_count(), 0);
        state.decide_focused(Decision::Rejected);
        assert_eq!(state.resolved_count(), state.hunks.len());
    }
}
