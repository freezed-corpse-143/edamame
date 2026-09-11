//! Line-level + inline-word diff over two strings, producing the [`Hunk`] sequence consumed by
//! [`crate::diff::DiffState`].  Wraps `similar::TextDiff::from_lines` so nothing else imports
//! `similar` directly, and adds two things on top:
//!
//! - Row-level table sub-diff: a hunk contained in a markdown table extent is split into per-row
//!   hunks (neighboring changed rows coalesced) so the user decides row by row.
//! - Stable [`HunkId`] allocation through a caller-supplied counter.
//!
//! Inline word-level highlights are restricted to text lines outside table cells and reach the
//! renderer through [`crate::diff::hunk::InlineSpan`].

use std::ops::Range;

use ropey::Rope;
use similar::{ChangeTag, TextDiff};

use crate::markdown::parse_offsets::{block_ranges_by, BlockKind};

use super::hunk::{Decision, Hunk, HunkId, HunkKind, InlineSide, InlineSpan};

/// Allocator for [`HunkId`] values.  Held by `DiffState` so a recompute mints fresh ids rather
/// than reusing old ones; stability across recomputes comes from old-side overlap matching
/// ([`match_by_old_overlap`]) instead.
#[derive(Debug, Default, Clone)]
pub struct HunkIdAllocator {
    next: u64,
}

impl HunkIdAllocator {
    pub fn new() -> Self {
        Self { next: 0 }
    }

    pub fn allocate(&mut self) -> HunkId {
        let id = HunkId(self.next);
        self.next = self.next.wrapping_add(1);
        id
    }
}

/// Outcome of [`compute`]: the hunk list plus advisory warnings for the UI.
#[derive(Debug, Default)]
pub struct HunkComputation {
    /// The hunks, in document order.
    pub hunks: Vec<Hunk>,
    /// A table had uneven cell counts and so was kept as line-level hunk(s) instead of per-row
    /// ones.  The UI flashes a hint so the coarse hunk doesn't read like a bug.
    pub uneven_table_fallback: bool,
}

/// Compute the hunk list for `old_text` vs `new_text`, in document order, with fresh ids from
/// `ids`.  Decisions are the caller's job.
///
/// Adjacent same-kind runs from `similar` coalesce, so a touching delete + insert produce one
/// `Replace`.  Hunks contained in a markdown table extent are then row-split via
/// [`split_table_hunk`].
pub fn compute(old_text: &str, new_text: &str, ids: &mut HunkIdAllocator) -> HunkComputation {
    let diff = TextDiff::from_lines(old_text, new_text);

    // First pass: collapse `similar`'s op groups into (old_lines, new_lines) ranges.
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut old_start: Option<usize> = None;
    let mut new_start: Option<usize> = None;
    let mut old_end: usize = 0;
    let mut new_end: usize = 0;
    let mut last_old_seen: usize = 0;
    let mut last_new_seen: usize = 0;

    let flush = |old_s: &mut Option<usize>,
                 new_s: &mut Option<usize>,
                 old_e: &mut usize,
                 new_e: &mut usize,
                 hunks: &mut Vec<Hunk>,
                 ids: &mut HunkIdAllocator| {
        if old_s.is_none() && new_s.is_none() {
            return;
        }
        let old_lines = old_s.unwrap_or(*old_e)..*old_e;
        let new_lines = new_s.unwrap_or(*new_e)..*new_e;
        if old_lines.start == old_lines.end && new_lines.start == new_lines.end {
            *old_s = None;
            *new_s = None;
            return;
        }
        let kind = Hunk::classify(&old_lines, &new_lines);
        hunks.push(Hunk {
            id: ids.allocate(),
            old_lines,
            new_lines,
            inline: Vec::new(),
            kind,
        });
        *old_s = None;
        *new_s = None;
    };

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                flush(
                    &mut old_start,
                    &mut new_start,
                    &mut old_end,
                    &mut new_end,
                    &mut hunks,
                    ids,
                );
                if let Some(i) = change.old_index() {
                    last_old_seen = i + 1;
                }
                if let Some(i) = change.new_index() {
                    last_new_seen = i + 1;
                }
            }
            ChangeTag::Delete => {
                if old_start.is_none() {
                    old_start = Some(change.old_index().unwrap_or(last_old_seen));
                    old_end = old_start.unwrap();
                }
                if new_start.is_none() {
                    // Mid-Replace, before any Insert is seen: anchor `new_end` at the last new
                    // line so a follow-up Insert starts in the right place.
                    new_end = last_new_seen;
                }
                old_end = change.old_index().map(|i| i + 1).unwrap_or(old_end + 1);
                last_old_seen = old_end;
            }
            ChangeTag::Insert => {
                if new_start.is_none() {
                    new_start = Some(change.new_index().unwrap_or(last_new_seen));
                    new_end = new_start.unwrap();
                }
                if old_start.is_none() {
                    old_end = last_old_seen;
                }
                new_end = change.new_index().map(|i| i + 1).unwrap_or(new_end + 1);
                last_new_seen = new_end;
            }
        }
    }
    flush(
        &mut old_start,
        &mut new_start,
        &mut old_end,
        &mut new_end,
        &mut hunks,
        ids,
    );

    // One index per side, so the passes below convert byte↔line without a `Rope` per hunk.
    let old_index = LineIndex::new(old_text);
    let new_index = LineIndex::new(new_text);

    // Second pass: inline highlights in `Replace` hunks.  `Insert` / `Delete` have no other side
    // to diff against; table hunks get theirs from the row sub-diff below.
    for h in &mut hunks {
        if h.kind == HunkKind::Replace {
            populate_inline_spans(h, &old_index, &new_index);
        }
    }

    // Third pass: row-level table sub-diff, for hunks contained in a table extent on *both*
    // sides.  One row-diff over the whole extent captures every changed row however the line-level
    // pass chunked it, so a table is split at most once and the other contained hunks are dropped
    // — re-splitting would emit every row twice, duplicating lines on resolve.
    //
    // When a new header+separator appears mid-table the lower fragment parses as its own table, so
    // one old extent's hunks can map to *different* new extents.  A single extent re-diff can't
    // represent two new tables, so that old extent is left un-split and its hunks pass through at
    // line level rather than being dropped — see
    // `table_split_into_two_keeps_both_changes_reviewable`.
    let old_table_extents = table_line_extents(&old_index);
    let new_table_extents = table_line_extents(&new_index);

    // Pre-scan each old extent for the new extent its contained hunks map to; `meta[i]` caches
    // `hunks[i]`'s per-side containment so the main loop needn't redo it.
    let meta: Vec<(Option<usize>, Option<usize>)> = hunks
        .iter()
        .map(|h| {
            (
                find_extent_idx(&old_table_extents, &h.old_lines),
                find_extent_idx(&new_table_extents, &h.new_lines),
            )
        })
        .collect();
    let mut ni_map = vec![NiMap::Unset; old_table_extents.len()];
    for &(oi, ni) in &meta {
        if let (Some(oi), Some(ni)) = (oi, ni) {
            ni_map[oi] = match ni_map[oi] {
                NiMap::Unset => NiMap::One(ni),
                NiMap::One(prev) if prev == ni => NiMap::One(prev),
                _ => NiMap::Conflict,
            };
        }
    }

    let mut split: Vec<Hunk> = Vec::with_capacity(hunks.len());
    let mut split_done = vec![false; old_table_extents.len()];
    let mut uneven_table_fallback = false;
    for (h, &(old_idx, new_idx)) in hunks.into_iter().zip(meta.iter()) {
        let (Some(oi), Some(ni)) = (old_idx, new_idx) else {
            // Not in a table on both sides: non-table and boundary-straddling hunks alike.
            split.push(h);
            continue;
        };
        if !matches!(ni_map[oi], NiMap::One(_)) {
            // Fragmented table: keep the line-level hunk so its change stays reviewable.
            split.push(h);
            continue;
        }
        if split_done[oi] {
            // Already row-split; drop this hunk so its rows aren't emitted twice.
            continue;
        }
        match split_table_hunk(
            &old_table_extents[oi],
            &new_table_extents[ni],
            &old_index,
            &new_index,
            ids,
        ) {
            SplitOutcome::Rows(rows) => {
                split_done[oi] = true;
                split.extend(rows);
            }
            SplitOutcome::Uneven => {
                // Uneven cells: review the table as a unit.  Deliberately not marked done — the
                // other contained hunks fall through here too, and with no whole-table coverage
                // there is nothing to duplicate.
                uneven_table_fallback = true;
                split.push(h);
            }
            SplitOutcome::Degenerate => {
                // Row-diff produced nothing (defensive); same non-marking rationale as above.
                split.push(h);
            }
        }
    }

    HunkComputation {
        hunks: split,
        uneven_table_fallback,
    }
}

/// Which new-side table extent one old extent's contained hunks map to.
#[derive(Clone, Copy)]
enum NiMap {
    /// No hunk is contained in this extent on both sides.
    Unset,
    /// Every contained hunk maps to the same new extent index.
    One(usize),
    /// Contained hunks map to *different* new extents (the table
    /// fragmented into several on the new side).
    Conflict,
}

/// Result of attempting to row-split a table extent.
enum SplitOutcome {
    /// Per-row hunks covering the whole table.
    Rows(Vec<Hunk>),
    /// Uneven cell counts: the caller keeps the line-level hunk(s) and flashes a hint.
    Uneven,
    /// Row-diff produced no hunks (defensive; shouldn't happen).
    Degenerate,
}

/// Populate `hunk.inline` with word-level spans, one line-pair at a time via
/// [`TextDiff::from_words`].  Lines that don't pair 1:1 across sides are skipped — the line-level
/// background highlight is signal enough there.
fn populate_inline_spans(hunk: &mut Hunk, old_index: &LineIndex, new_index: &LineIndex) {
    let old_lines = old_index.slice(hunk.old_lines.clone());
    let new_lines = new_index.slice(hunk.new_lines.clone());
    let pair_count = old_lines.len().min(new_lines.len());
    let mut spans = Vec::new();
    for i in 0..pair_count {
        let old_line = old_lines[i];
        let new_line = new_lines[i];
        if old_line == new_line {
            continue;
        }
        let word_diff = TextDiff::from_words(old_line, new_line);
        let mut old_pos = 0usize;
        let mut new_pos = 0usize;
        for change in word_diff.iter_all_changes() {
            let text = change.value();
            let char_count = text.chars().count();
            match change.tag() {
                ChangeTag::Equal => {
                    old_pos += char_count;
                    new_pos += char_count;
                }
                ChangeTag::Delete => {
                    if char_count > 0 {
                        spans.push(InlineSpan {
                            side: InlineSide::Old,
                            line_in_hunk: i,
                            chars: old_pos..old_pos + char_count,
                        });
                    }
                    old_pos += char_count;
                }
                ChangeTag::Insert => {
                    if char_count > 0 {
                        spans.push(InlineSpan {
                            side: InlineSide::New,
                            line_in_hunk: i,
                            chars: new_pos..new_pos + char_count,
                        });
                    }
                    new_pos += char_count;
                }
            }
        }
    }
    hunk.inline = spans;
}

/// Line-start byte offsets, mirroring ropey's line model (N newlines → N+1 lines, with a final
/// empty line when the text ends in `\n`).  Built once per side so per-hunk slicing and byte↔line
/// conversion needn't rebuild a `Rope`.
struct LineIndex<'a> {
    text: &'a str,
    /// First byte of each line; strictly increasing, so [`Self::byte_to_line`] can binary-search.
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    fn new(text: &'a str) -> Self {
        let mut starts = vec![0usize];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self { text, starts }
    }

    fn len_lines(&self) -> usize {
        self.starts.len()
    }

    /// Line containing `byte`.  Mirrors `Rope::byte_to_line`, `text.len()` behavior included.
    fn byte_to_line(&self, byte: usize) -> usize {
        match self.starts.binary_search(&byte) {
            Ok(i) => i,
            // `Err(0)` is impossible: `starts[0] == 0 <= byte` always.
            Err(i) => i - 1,
        }
    }

    /// Text of each line in `range` without its trailing `\n`, clamped to the available lines.
    fn slice(&self, range: Range<usize>) -> Vec<&'a str> {
        if range.start >= range.end {
            return Vec::new();
        }
        let total = self.len_lines();
        let end = range.end.min(total);
        let start = range.start.min(end);
        let mut out = Vec::with_capacity(end - start);
        for i in start..end {
            let line_start = self.starts[i];
            let line_end = if i + 1 < total {
                self.starts[i + 1]
            } else {
                self.text.len()
            };
            let raw = &self.text[line_start..line_end];
            out.push(raw.strip_suffix('\n').unwrap_or(raw));
        }
        out
    }
}

/// Extent of one table block as line indices in the source text.
#[derive(Debug, Clone)]
struct TableExtent {
    /// Half-open line range.
    lines: Range<usize>,
}

/// Each table's line range, via [`block_ranges_by`].
fn table_line_extents(index: &LineIndex) -> Vec<TableExtent> {
    block_ranges_by(index.text, |kind| kind == BlockKind::Table)
        .into_iter()
        .map(|r| TableExtent {
            lines: index.byte_to_line(r.start)..index.byte_to_line(r.end),
        })
        .collect()
}

/// Row-split one table: a single `similar` diff over its rows, one hunk per coalesced run of
/// changed rows.  [`SplitOutcome::Uneven`] when the table is non-rectangular.
///
/// The caller must already have verified via [`find_extent_idx`] that the triggering hunk is fully
/// contained in these extents on both sides.
fn split_table_hunk(
    old_extent: &TableExtent,
    new_extent: &TableExtent,
    old_index: &LineIndex,
    new_index: &LineIndex,
    ids: &mut HunkIdAllocator,
) -> SplitOutcome {
    // Every row on a side must have the same cell count, and the maxima must match across sides.
    let old_rows = old_index.slice(old_extent.lines.clone());
    let new_rows = new_index.slice(new_extent.lines.clone());
    if !table_rows_uniform(&old_rows, &new_rows) {
        return SplitOutcome::Uneven;
    }

    // Rows only.  Decisions are per-row, but neighboring changed rows coalesce into one hunk.
    let old_joined: String = old_rows.iter().map(|s| format!("{s}\n")).collect();
    let new_joined: String = new_rows.iter().map(|s| format!("{s}\n")).collect();
    let row_diff = TextDiff::from_lines(&old_joined, &new_joined);

    let mut hunks: Vec<Hunk> = Vec::new();
    let mut run_old_start: Option<usize> = None;
    let mut run_new_start: Option<usize> = None;
    let mut run_old_end: usize = 0;
    let mut run_new_end: usize = 0;
    let mut last_old: usize = 0;
    let mut last_new: usize = 0;

    let emit = |run_old_s: &mut Option<usize>,
                run_new_s: &mut Option<usize>,
                run_old_e: &mut usize,
                run_new_e: &mut usize,
                hunks: &mut Vec<Hunk>,
                ids: &mut HunkIdAllocator| {
        if run_old_s.is_none() && run_new_s.is_none() {
            return;
        }
        let o = run_old_s.unwrap_or(*run_old_e)..*run_old_e;
        let n = run_new_s.unwrap_or(*run_new_e)..*run_new_e;
        if o.start == o.end && n.start == n.end {
            *run_old_s = None;
            *run_new_s = None;
            return;
        }
        let kind = Hunk::classify(&o, &n);
        hunks.push(Hunk {
            id: ids.allocate(),
            old_lines: (old_extent.lines.start + o.start)..(old_extent.lines.start + o.end),
            new_lines: (new_extent.lines.start + n.start)..(new_extent.lines.start + n.end),
            inline: Vec::new(),
            kind,
        });
        *run_old_s = None;
        *run_new_s = None;
    };

    for change in row_diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                emit(
                    &mut run_old_start,
                    &mut run_new_start,
                    &mut run_old_end,
                    &mut run_new_end,
                    &mut hunks,
                    ids,
                );
                if let Some(i) = change.old_index() {
                    last_old = i + 1;
                }
                if let Some(i) = change.new_index() {
                    last_new = i + 1;
                }
            }
            ChangeTag::Delete => {
                if run_old_start.is_none() {
                    run_old_start = Some(change.old_index().unwrap_or(last_old));
                    run_old_end = run_old_start.unwrap();
                }
                if run_new_start.is_none() {
                    run_new_end = last_new;
                }
                run_old_end = change.old_index().map(|i| i + 1).unwrap_or(run_old_end + 1);
                last_old = run_old_end;
            }
            ChangeTag::Insert => {
                if run_new_start.is_none() {
                    run_new_start = Some(change.new_index().unwrap_or(last_new));
                    run_new_end = run_new_start.unwrap();
                }
                if run_old_start.is_none() {
                    run_old_end = last_old;
                }
                run_new_end = change.new_index().map(|i| i + 1).unwrap_or(run_new_end + 1);
                last_new = run_new_end;
            }
        }
    }
    emit(
        &mut run_old_start,
        &mut run_new_start,
        &mut run_old_end,
        &mut run_new_end,
        &mut hunks,
        ids,
    );

    // Word-level inline highlights inside each Replace row-hunk.
    for h in &mut hunks {
        if h.kind == HunkKind::Replace {
            populate_inline_spans(h, old_index, new_index);
        }
    }

    // Zero hunks should be unreachable (the parent hunk exists because something differed), but
    // report it so the caller keeps the monolithic hunk rather than dropping the change.
    if hunks.is_empty() {
        return SplitOutcome::Degenerate;
    }

    SplitOutcome::Rows(hunks)
}

/// The table extent that *fully contains* `lines`.
///
/// Containment, not overlap: [`split_table_hunk`] re-diffs the whole extent, so row-splitting a
/// hunk that also covers lines outside the table would silently drop those lines from its output —
/// losing a reviewable change and corrupting the merge.  A straddling hunk stays monolithic.
fn find_extent_idx(extents: &[TableExtent], lines: &Range<usize>) -> Option<usize> {
    extents
        .iter()
        .position(|e| lines.start >= e.lines.start && lines.end <= e.lines.end)
}

fn table_rows_uniform(old_rows: &[&str], new_rows: &[&str]) -> bool {
    // A zero-row side can't be uniformity-matched, and falling through lets `max_old == max_new
    // == 0` pass the final guard, which makes `split_table_hunk` drop the hunk from its output.
    if old_rows.is_empty() || new_rows.is_empty() {
        return false;
    }
    fn cell_count(row: &str) -> Option<usize> {
        let trimmed = row.trim();
        if !trimmed.starts_with('|') {
            return None;
        }
        // Unescaped `|` only; leading and trailing delimiters are common but not required.
        let mut pipes = 0usize;
        let mut chars = trimmed.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                let _ = chars.next();
                continue;
            }
            if c == '|' {
                pipes += 1;
            }
        }
        // N pipes → N-1 cells with both outer delimiters (pulldown-cmark's canonical form),
        // else N cells.
        let cells = if trimmed.ends_with('|') {
            pipes.saturating_sub(1)
        } else {
            pipes
        };
        Some(cells)
    }

    let mut max_old = 0usize;
    for row in old_rows {
        // Separator rows count like data rows: uniformity spans header, separator and data.
        let Some(c) = cell_count(row) else {
            return false;
        };
        if max_old == 0 {
            max_old = c;
        } else if c != max_old {
            return false;
        }
    }
    let mut max_new = 0usize;
    for row in new_rows {
        let Some(c) = cell_count(row) else {
            return false;
        };
        if max_new == 0 {
            max_new = c;
        } else if c != max_new {
            return false;
        }
    }
    max_old == max_new
}

/// A `Vec<Decision>` of `hunks.len()` seeded to `Pending`; used at every recompute site.
pub fn pending_decisions(hunks: &[Hunk]) -> Vec<Decision> {
    vec![Decision::Pending; hunks.len()]
}

/// Overlapping lines between two half-open ranges; `0` when disjoint.
fn old_range_overlap(a: &Range<usize>, b: &Range<usize>) -> usize {
    let start = a.start.max(b.start);
    let end = a.end.min(b.end);
    end.saturating_sub(start)
}

/// Match `hunk` against a prior hunk list by **old-side overlap** — the id-stability primitive
/// shared by the reconcile path and the post-edit recompute.
///
/// The old side is invariant for the life of a review (external writes and in-diff edits both only
/// change the new side), so it is a stable anchor.  The prior overlapping `hunk.old_lines` most
/// wins; ties break toward the smallest `old_lines.start`.
///
/// **Insert hunks** have an empty old-side range and so overlap nothing; they are anchored by
/// insertion point instead, which keeps an accepted insertion's decision across an unrelated
/// external write (an agent adds a block, the user accepts it, the agent edits elsewhere).
/// Distinct Inserts sit at distinct old positions, so the anchor is unambiguous, and any real
/// overlap (score ≥ 1) outranks an insertion-point match (score 0).
pub fn match_by_old_overlap(hunk: &Hunk, priors: &[Hunk]) -> Option<usize> {
    let cand = &hunk.old_lines;
    let cand_empty = cand.start == cand.end;
    let mut best: Option<(usize, usize, usize)> = None; // (score, start, idx)
    for (i, p) in priors.iter().enumerate() {
        let po = &p.old_lines;
        let overlap = old_range_overlap(cand, po);
        let score = if overlap > 0 {
            overlap
        } else if cand_empty && po.start == po.end && po.start == cand.start {
            // Same insertion point.  Score 0, so any real overlap still wins.
            0
        } else {
            continue;
        };
        let better = match best {
            None => true,
            Some((best_score, best_start, _)) => {
                score > best_score || (score == best_score && po.start < best_start)
            }
        };
        if better {
            best = Some((score, po.start, i));
        }
    }
    best.map(|(_, _, idx)| idx)
}

/// The hunk's new-side text, trailing newlines included (`""` for a `Delete`).  The reconcile gate
/// compares it to decide whether an external write changed a matched hunk's target.
pub fn hunk_new_side_text(hunk: &Hunk, rope: &Rope) -> String {
    let mut out = String::new();
    let total = rope.len_lines();
    for line_idx in hunk.new_lines.clone() {
        if line_idx < total {
            out.push_str(&rope.line(line_idx).to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> HunkIdAllocator {
        HunkIdAllocator::new()
    }

    /// The hunks of [`compute`], discarding warnings these cases don't assert on.
    fn compute_hunks(old: &str, new: &str, ids: &mut HunkIdAllocator) -> Vec<Hunk> {
        compute(old, new, ids).hunks
    }

    #[test]
    fn identical_inputs_produce_no_hunks() {
        let s = "a\nb\nc\n";
        let mut a = ids();
        assert!(compute_hunks(s, s, &mut a).is_empty());
    }

    #[test]
    fn pure_insert_produces_one_insert_hunk() {
        let old = "a\nb\n";
        let new = "a\nb\nc\n";
        let mut a = ids();
        let hunks = compute_hunks(old, new, &mut a);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind, HunkKind::Insert);
        assert_eq!(hunks[0].old_lines, 2..2);
        assert_eq!(hunks[0].new_lines, 2..3);
    }

    #[test]
    fn pure_delete_produces_one_delete_hunk() {
        let old = "a\nb\nc\n";
        let new = "a\nc\n";
        let mut a = ids();
        let hunks = compute_hunks(old, new, &mut a);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind, HunkKind::Delete);
        assert_eq!(hunks[0].old_lines, 1..2);
    }

    #[test]
    fn replace_emits_inline_spans_on_paired_lines() {
        let old = "alpha bravo\n";
        let new = "alpha gamma\n";
        let mut a = ids();
        let hunks = compute_hunks(old, new, &mut a);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind, HunkKind::Replace);
        assert!(!hunks[0].inline.is_empty());
    }

    fn hunk_with_old(old_lines: Range<usize>) -> Hunk {
        Hunk {
            id: HunkId(0),
            old_lines: old_lines.clone(),
            new_lines: 0..0,
            inline: Vec::new(),
            kind: Hunk::classify(&old_lines, &(0..0)),
        }
    }

    #[test]
    fn match_by_old_overlap_picks_largest_overlap_and_breaks_ties() {
        // Largest overlap wins.
        let priors = vec![hunk_with_old(0..3), hunk_with_old(5..7)];
        let cand = hunk_with_old(1..2);
        assert_eq!(match_by_old_overlap(&cand, &priors), Some(0));

        // Tie on overlap → smallest old_lines.start wins.
        let priors = vec![hunk_with_old(0..4), hunk_with_old(2..6)];
        let cand = hunk_with_old(2..4); // overlaps both by 2 lines
        assert_eq!(match_by_old_overlap(&cand, &priors), Some(0));

        // No overlap → None.
        let priors = vec![hunk_with_old(0..2)];
        assert_eq!(match_by_old_overlap(&hunk_with_old(10..12), &priors), None);
        // An empty candidate must not match a non-empty prior straddling the insertion point.
        assert_eq!(match_by_old_overlap(&hunk_with_old(1..1), &priors), None);
    }

    #[test]
    fn match_by_old_overlap_anchors_inserts_by_position() {
        // Two prior Inserts at distinct old-side positions.
        let priors = vec![hunk_with_old(1..1), hunk_with_old(3..3)];
        // A candidate Insert at the same anchor matches that prior.
        assert_eq!(match_by_old_overlap(&hunk_with_old(3..3), &priors), Some(1));
        assert_eq!(match_by_old_overlap(&hunk_with_old(1..1), &priors), Some(0));
        // An Insert at a fresh position matches nothing.
        assert_eq!(match_by_old_overlap(&hunk_with_old(5..5), &priors), None);
        // A real overlap outranks any insertion-point match.
        let priors = vec![hunk_with_old(2..2), hunk_with_old(1..4)];
        assert_eq!(match_by_old_overlap(&hunk_with_old(2..3), &priors), Some(1));
    }

    #[test]
    fn ids_are_unique_and_monotonic() {
        let old = "a\nb\nc\n";
        let new = "x\nb\ny\n";
        let mut a = ids();
        let hunks = compute_hunks(old, new, &mut a);
        let mut seen = std::collections::HashSet::new();
        for h in &hunks {
            assert!(seen.insert(h.id), "duplicate id");
        }
    }
}
