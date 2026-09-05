//! Column-width computation and cell text wrapping for GFM tables; the box-drawing paint
//! lives in `renderer::render_table`.  Also parses and emits the per-table
//! `<!-- tui-columns: [...] -->` comment that persists user-set widths.
//!
//! Widths are in *terminal columns*, measured with `unicode-width` (CJK / wide chars).
//!
//! # Width calculation strategy — min-max proportional
//!
//! The algorithm browsers use for `table-layout: auto`.  Per column, `min = longest word`
//! and `max = longest cell`; then:
//!
//! 1. Every `max` fits the budget → use the `max` widths.
//! 2. Otherwise, if the `min`s fit → distribute the slack across unpinned columns weighted
//!    by `(max - min)`, so prose columns absorb it and `min == max` columns don't move.
//! 3. Otherwise every column drops to its `min` and the table overflows horizontally — a
//!    *prose* word is never broken.  Cells with code spans or links report a reduced `min`
//!    (`renderer::table::cell_min_width`) and hard-split instead of forcing the column wide.
//!
//! A `Some(w)` entry in `user_widths` pins that column and excludes it from the
//! distribution, so drag-set widths survive viewport pressure.  Cells wider than their
//! column word-wrap; a row's height is the maximum wrap count across its cells.

use std::num::NonZeroUsize;

use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

/// Minimum column width — narrower leaves no room for a `...` truncation indicator.
pub const MIN_COL_WIDTH: usize = 3;

/// Per-column overhead for `│ content ` — the separator plus one space each side.  The
/// trailing `│` at row end is [`ROW_END_OVERHEAD`].
pub const PER_COL_OVERHEAD: usize = 3;
pub const ROW_END_OVERHEAD: usize = 1;

/// Compute column widths by the min-max proportional strategy described at module level.
///
/// `cell_max_widths[row][col]` is the cell's full width, `cell_min_widths[row][col]` its
/// longest single word.  `viewport_width` is the total budget including borders and
/// padding; `usize::MAX` disables distribution and always returns `max` widths.
/// `user_widths` entries of `Some(w)` pin a column (clamped to `MIN_COL_WIDTH`) and take it
/// out of the distribution; its length must match `col_count`.
///
/// Returns `col_count` widths; when not even the `min`s fit, returns the `min`s and leaves
/// truncation-versus-overflow to the caller.
pub fn compute_widths(
    cell_max_widths: &[Vec<usize>],
    cell_min_widths: &[Vec<usize>],
    col_count: usize,
    viewport_width: usize,
    user_widths: Option<&[Option<usize>]>,
) -> Vec<usize> {
    if col_count == 0 {
        return Vec::new();
    }

    // Clamped to MIN_COL_WIDTH so a column never collapses below ellipsis room.
    let mut col_max = vec![MIN_COL_WIDTH; col_count];
    let mut col_min = vec![MIN_COL_WIDTH; col_count];
    for row in cell_max_widths {
        for (i, w) in row.iter().take(col_count).enumerate() {
            col_max[i] = col_max[i].max(*w);
        }
    }
    for row in cell_min_widths {
        for (i, w) in row.iter().take(col_count).enumerate() {
            col_min[i] = col_min[i].max(*w);
        }
    }
    for i in 0..col_count {
        if col_min[i] > col_max[i] {
            col_max[i] = col_min[i];
        }
    }

    let mut widths = col_max.clone();
    let mut pinned = vec![false; col_count];
    if let Some(uw) = user_widths {
        for (i, w) in uw.iter().take(col_count).enumerate() {
            if let Some(val) = w {
                widths[i] = (*val).max(MIN_COL_WIDTH);
                pinned[i] = true;
            }
        }
    }

    let border_budget = PER_COL_OVERHEAD * col_count + ROW_END_OVERHEAD;
    let pinned_total: usize = widths
        .iter()
        .enumerate()
        .filter(|(i, _)| pinned[*i])
        .map(|(_, w)| *w)
        .sum();

    // With `viewport_width == usize::MAX` this stays `usize::MAX`, so the natural-fit
    // branch always wins.
    let available = viewport_width
        .saturating_sub(border_budget)
        .saturating_sub(pinned_total);

    let unpinned: Vec<usize> = (0..col_count).filter(|i| !pinned[*i]).collect();
    let unpinned_max_total: usize = unpinned.iter().map(|i| col_max[*i]).sum();
    let unpinned_min_total: usize = unpinned.iter().map(|i| col_min[*i]).sum();

    if unpinned_max_total <= available {
        for &i in &unpinned {
            widths[i] = col_max[i];
        }
    } else if unpinned_min_total <= available {
        let slack = available - unpinned_min_total;
        let total_weight: usize = unpinned.iter().map(|i| col_max[*i] - col_min[*i]).sum();
        if let Some(total_weight) = NonZeroUsize::new(total_weight) {
            // Weighted integer division; the leftover cells go to the largest residuals
            // below, so every available cell is used.
            let mut residuals: Vec<(usize, usize)> = Vec::with_capacity(unpinned.len());
            let mut assigned = 0usize;
            for &i in &unpinned {
                let weight = col_max[i] - col_min[i];
                let numer = slack * weight;
                let extra = numer / total_weight;
                let remainder = numer % total_weight;
                widths[i] = col_min[i] + extra;
                assigned += extra;
                residuals.push((i, remainder));
            }
            // Descending residual, ties broken on index so the result is deterministic.
            residuals.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let mut leftover = slack.saturating_sub(assigned);
            for (i, _) in residuals {
                if leftover == 0 {
                    break;
                }
                if widths[i] < col_max[i] {
                    widths[i] += 1;
                    leftover -= 1;
                }
            }
        } else {
            // Defensive only: zero total weight means every `min == max`, which the
            // natural-fit branch above would already have claimed.  Kept so a change to
            // either total can't turn a divide-by-zero into a panic.
            for &i in &unpinned {
                widths[i] = col_min[i];
            }
        }
    } else {
        // Even the mins don't fit; the caller truncates or accepts overflow.
        for &i in &unpinned {
            widths[i] = col_min[i];
        }
    }

    widths
}

/// Wrap a cell's plain text to `width` terminal columns, one `String` per visual row.  A
/// single word longer than `width` is hard-split.  Never empty — empty input yields one
/// empty row.
pub fn wrap_cell(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_owned()];
    }
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut rows = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;

    for word in split_soft(text) {
        let w = UnicodeWidthStr::width(word.as_str());
        if current.is_empty() {
            if w <= width {
                current.push_str(&word);
                current_w = w;
            } else {
                for chunk in hard_split(&word, width) {
                    rows.push(chunk);
                }
                current.clear();
                current_w = 0;
            }
        } else {
            if current_w + w <= width {
                current.push_str(&word);
                current_w += w;
            } else {
                rows.push(std::mem::take(&mut current));
                let w_trimmed = word.trim_start();
                let w_w = UnicodeWidthStr::width(w_trimmed);
                if w_w <= width {
                    current.push_str(w_trimmed);
                    current_w = w_w;
                } else {
                    for chunk in hard_split(w_trimmed, width) {
                        rows.push(chunk);
                    }
                    current_w = 0;
                }
            }
        }
    }
    if !current.is_empty() {
        rows.push(current);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// [`wrap_cell`] plus the char index in `text` where each row begins — `RenderedView` maps
/// a cursor offset inside a wrapped cell back to a (sub-line, column) with it.
///
/// Whitespace at a break point is drawn on neither row, so continuation rows start at the
/// first non-whitespace char after the previous row, and a cursor on a dropped space maps
/// to the next row's start.
pub fn wrap_cell_with_indices(text: &str, width: usize) -> Vec<(usize, String)> {
    let rows = wrap_cell(text, width);
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<(usize, String)> = Vec::with_capacity(rows.len());
    let mut idx = 0;
    for (i, row) in rows.into_iter().enumerate() {
        if i > 0 {
            while idx < chars.len() && chars[idx].is_whitespace() {
                idx += 1;
            }
        }
        let row_start = idx;
        idx += row.chars().count();
        out.push((row_start, row));
    }
    out
}

/// Split `text` into tokens of "whitespace run + following word", keeping the whitespace
/// attached so [`wrap_cell`] can re-include spaces exactly as they appeared.
fn split_soft(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_ws = true;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_ws && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(ch);
            in_ws = true;
        } else {
            if in_ws && !cur.is_empty() {
                // keep leading whitespace attached to this word
            }
            cur.push(ch);
            in_ws = false;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Hard-split a word across rows of `width` terminal cells, preferring a break just after
/// a punctuation character ([`is_break_after`]) so identifiers, paths, and URLs don't split
/// mid-token.  Never empty.
fn hard_split(word: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut cur: Vec<char> = Vec::new();
    let mut cur_w = 0usize;
    for ch in word.chars() {
        let cw = UnicodeWidthStr::width(ch.to_string().as_str());
        if cur_w + cw > width && !cur.is_empty() {
            let cut = preferred_cut(cur.len(), |i| cur[i]);
            rows.push(cur[..cut].iter().collect());
            cur.drain(..cut);
            cur_w = cur
                .iter()
                .map(|c| UnicodeWidthStr::width(c.to_string().as_str()))
                .sum();
        }
        cur.push(ch);
        cur_w += cw;
    }
    if !cur.is_empty() {
        rows.push(cur.iter().collect());
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// Characters a hard-split prefers to break *after* — tuned for identifiers, paths, URLs.
pub fn is_break_after(ch: char) -> bool {
    matches!(
        ch,
        '_' | '-'
            | '.'
            | ','
            | ';'
            | ':'
            | '/'
            | '\\'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '='
            | '&'
            | '?'
            | '#'
            | '@'
    )
}

/// Cut index for a hard-split chunk of `len` chars (exposed via `char_at`): just after the
/// last [`is_break_after`] character in the *trailing half*, else `len`.  Limiting the scan
/// to the trailing half keeps every row at least half-width, so a punctuation-dense token
/// can't degenerate into confetti rows.  Shared with the styled counterpart in
/// `renderer::util`.
pub fn preferred_cut(len: usize, char_at: impl Fn(usize) -> char) -> usize {
    let lookback = len / 2;
    for back in 0..lookback {
        let i = len - 1 - back;
        if is_break_after(char_at(i)) {
            return i + 1;
        }
    }
    len
}

// ── Column-width comment parsing ────────────────────────────────────────────

/// Parse a `<!-- tui-columns: [20, _, 30] -->` comment: `Some(w)` per pinned column,
/// `None` per `_` placeholder.  `None` overall when absent, malformed, or all-`_` (which
/// carries no information).
pub fn parse_column_widths_comment(text: &str) -> Option<Vec<Option<usize>>> {
    let start = text.find("<!-- tui-columns:")?;
    let after = &text[start + "<!-- tui-columns:".len()..];
    let end_rel = after.find("-->")?;
    let body = after[..end_rel].trim();
    let inner = body.strip_prefix('[')?.strip_suffix(']')?;
    let mut widths = Vec::new();
    for part in inner.split(',') {
        let s = part.trim();
        if s.is_empty() {
            continue;
        }
        if s == "_" {
            widths.push(None);
        } else {
            widths.push(Some(s.parse::<usize>().ok()?));
        }
    }
    if widths.is_empty() || widths.iter().all(Option::is_none) {
        None
    } else {
        Some(widths)
    }
}

/// Emit a `<!-- tui-columns: [20, _, 30] -->` comment; `None` becomes `_` (auto-sized).
pub fn format_column_widths_comment(widths: &[Option<usize>]) -> String {
    let body: Vec<String> = widths
        .iter()
        .map(|w| match w {
            Some(v) => v.to_string(),
            None => "_".to_owned(),
        })
        .collect();
    format!("<!-- tui-columns: [{}] -->", body.join(", "))
}

// ── Pipe position / cell-range helpers ──────────────────────────────────────
//
// Shared by `TableView`'s mouse hit-testing and `RenderedView`'s cell-scoped raw reveal.
// Pure functions of the raw row text and the rendered `Line` — no editor-state coupling.

/// Char positions of unescaped `|` in a raw table row (GFM: `\|` escapes, `\\|` does not).
pub fn raw_pipe_positions(row: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut escaped = false;
    for (i, ch) in row.chars().enumerate() {
        if ch == '|' && !escaped {
            positions.push(i);
            escaped = false;
        } else if ch == '\\' {
            escaped = !escaped;
        } else {
            escaped = false;
        }
    }
    positions
}

/// Char positions of `│` box-drawing pipe characters in a rendered line.
pub fn rendered_pipe_positions(line: &Line<'_>) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut col = 0usize;
    for span in &line.spans {
        for ch in span.content.chars() {
            if ch == '│' {
                positions.push(col);
            }
            col += 1;
        }
    }
    positions
}

/// Map a raw char column to the matching rendered column, aligning the two pipe sequences.
/// `None` when the pipe counts disagree (alignment row, border).
pub fn table_raw_col_to_rendered_col(
    raw_row: &str,
    rendered_line: &Line<'_>,
    raw_col: usize,
) -> Option<usize> {
    let raw_pipes = raw_pipe_positions(raw_row);
    let rendered_pipes = rendered_pipe_positions(rendered_line);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return None;
    }
    let col_count = raw_pipes.len() - 1;

    // Cell `i` spans (raw_pipes[i] + 1) .. raw_pipes[i + 1].
    let cell_idx = (0..col_count)
        .find(|&i| raw_col < raw_pipes[i + 1])
        .unwrap_or(col_count - 1);
    let raw_cell_start = raw_pipes[cell_idx] + 1;
    let rend_cell_start = rendered_pipes[cell_idx] + 1;
    let rend_cell_end = rendered_pipes[cell_idx + 1];

    let raw_offset_in_cell = raw_col.saturating_sub(raw_cell_start);
    let raw_cell_text: String = raw_row
        .chars()
        .skip(raw_cell_start)
        .take(raw_pipes[cell_idx + 1].saturating_sub(raw_cell_start))
        .collect();
    let raw_leading = raw_cell_text
        .chars()
        .take_while(|c| c.is_whitespace())
        .count();
    // Rendered cell is `<space><content><pad><space>`, so a click in the raw content
    // region maps to 1 + (offset past the raw leading whitespace).
    let rend_offset_in_cell = if raw_offset_in_cell <= raw_leading {
        0
    } else {
        1 + (raw_offset_in_cell - raw_leading)
    };
    let rend_cell_width = rend_cell_end.saturating_sub(rend_cell_start);
    Some(rend_cell_start + rend_offset_in_cell.min(rend_cell_width))
}

/// Map a raw char-column range to the rendered segments visible on wrap-chunk `sub`.
/// Cells wrap independently, so each contributes at most one segment.  Used by the
/// selection / search overlay painter on continuation sub-lines, where
/// [`table_raw_col_to_rendered_col`]'s first-chunk mapping doesn't apply.  Chunk layout is
/// computed over raw cell text while the renderer wraps marker-stripped chars, so segments
/// are approximate for styled cells.  Empty when the pipe sequences don't match.
pub fn table_raw_col_range_to_rendered_segments(
    raw_row: &str,
    rendered_line: &Line<'_>,
    raw_start: usize,
    raw_end: usize,
    sub: usize,
) -> Vec<(usize, usize)> {
    let raw_pipes = raw_pipe_positions(raw_row);
    let rendered_pipes = rendered_pipe_positions(rendered_line);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return Vec::new();
    }
    let col_count = raw_pipes.len() - 1;
    let raw_chars: Vec<char> = raw_row.chars().collect();
    let mut out = Vec::new();
    for i in 0..col_count {
        let raw_cell_start = raw_pipes[i] + 1;
        let raw_cell_end = raw_pipes[i + 1];
        let cell_chars = &raw_chars[raw_cell_start..raw_cell_end];
        let leading = cell_chars.iter().take_while(|c| c.is_whitespace()).count();
        let trailing = cell_chars
            .iter()
            .rev()
            .take_while(|c| c.is_whitespace())
            .count();
        let content_len = cell_chars.len().saturating_sub(leading + trailing);
        let trimmed: String = cell_chars[leading..leading + content_len].iter().collect();
        let width = rendered_pipes[i + 1]
            .saturating_sub(rendered_pipes[i] + 3)
            .max(1);
        let chunks = wrap_cell_with_indices(&trimmed, width);
        let Some((chunk_start, chunk_text)) = chunks.get(sub) else {
            continue;
        };
        let lo_raw = raw_cell_start + leading + chunk_start;
        let hi_raw = lo_raw + chunk_text.chars().count();
        let s = raw_start.max(lo_raw);
        let e = raw_end.min(hi_raw);
        if s >= e {
            continue;
        }
        let rend_chunk_start = rendered_pipes[i] + 2;
        out.push((
            rend_chunk_start + (s - lo_raw),
            rend_chunk_start + (e - lo_raw),
        ));
    }
    out
}

/// Metadata for overlaying a raw cell on a rendered table row.  `rendered_start..
/// rendered_end` spans the content area between the two `│` characters, exclusive;
/// `raw_text` is padded/clamped to that width when painted so borders and neighboring
/// cells stay intact.
pub struct CellOverlay {
    pub rendered_start: usize,
    pub rendered_end: usize,
    pub raw_text: String,
    /// Cursor offset within `raw_text` in chars; `None` when it sits outside the overlay
    /// area, which means the caller takes the fallback path.
    pub cursor_in_cell: Option<usize>,
    /// Byte offset in the raw row where this cell's content starts, so the caller can
    /// align an absolute selection byte range onto `raw_text`.
    pub raw_cell_byte_start: usize,
}

/// A cell-scoped overlay for the cursor's active cell.
///
/// `None` when the row isn't a table row, when the pipe counts disagree (the alignment row
/// renders as `├─┼─┤`), or when the raw cell text is wider than the rendered area — the
/// caller then falls back to the full row reveal.
pub fn compute_cell_overlay(
    raw_row: &str,
    rendered_line: &Line<'_>,
    cursor_col: usize,
) -> Option<CellOverlay> {
    let raw_pipes = raw_pipe_positions(raw_row);
    let rendered_pipes = rendered_pipe_positions(rendered_line);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return None;
    }

    // Pipes at or before the cursor, minus one (pipe 0 begins cell 0).
    let col_count = raw_pipes.len() - 1;
    let preceding = raw_pipes.iter().take_while(|&&p| p < cursor_col).count();
    let cell_idx = preceding.saturating_sub(1).min(col_count - 1);

    let raw_cell_start = raw_pipes[cell_idx] + 1;
    let raw_cell_end = raw_pipes[cell_idx + 1];
    let raw_text: String = raw_row
        .chars()
        .skip(raw_cell_start)
        .take(raw_cell_end - raw_cell_start)
        .collect();

    let raw_cell_byte_start = raw_row
        .char_indices()
        .nth(raw_cell_start)
        .map(|(b, _)| b)
        .unwrap_or(raw_row.len());

    let rendered_start = rendered_pipes[cell_idx] + 1;
    let rendered_end = rendered_pipes[cell_idx + 1];
    let rendered_width = rendered_end.saturating_sub(rendered_start);

    if raw_text.chars().count() > rendered_width {
        return None;
    }

    let cursor_offset = cursor_col.saturating_sub(raw_cell_start);
    let cursor_in_cell = if cursor_offset < rendered_width {
        Some(cursor_offset)
    } else if cursor_offset == rendered_width {
        Some(rendered_width.saturating_sub(1))
    } else {
        None
    };

    Some(CellOverlay {
        rendered_start,
        rendered_end,
        raw_text,
        cursor_in_cell,
        raw_cell_byte_start,
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// mins == maxes (single-word cells), exercising the "no-wrap" path.
    fn maxes_eq_mins(cells: Vec<Vec<usize>>) -> (Vec<Vec<usize>>, Vec<Vec<usize>>) {
        let mins = cells.clone();
        (cells, mins)
    }

    #[test]
    fn compute_widths_uses_natural_when_room_available() {
        let (maxes, mins) = maxes_eq_mins(vec![vec![2, 4, 3], vec![3, 5, 2]]);
        let widths = compute_widths(&maxes, &mins, 3, 80, None);
        assert_eq!(widths, vec![3, 5, 3]); // clamped to MIN where natural was 2
    }

    #[test]
    fn compute_widths_returns_mins_when_max_exceeds_budget_and_no_slack() {
        // Single-word cells: min == max, so no slack — the table overflows rather than
        // truncating.
        let (maxes, mins) = maxes_eq_mins(vec![vec![10, 10, 10]]);
        let widths = compute_widths(&maxes, &mins, 3, 20, None);
        assert_eq!(widths, vec![10, 10, 10]);
        assert!(widths.iter().all(|w| *w >= MIN_COL_WIDTH));
    }

    #[test]
    fn compute_widths_distributes_slack_proportionally_to_max_minus_min() {
        // Col 0 is prose (max 20, min 4); col 1 has no flexibility (min == max == 5), so
        // col 0 absorbs all the slack.
        let maxes = vec![vec![20, 5]];
        let mins = vec![vec![4, 5]];
        // border 7, viewport 22 → slack 6, all of it weighted onto col 0.
        let widths = compute_widths(&maxes, &mins, 2, 22, None);
        assert_eq!(widths, vec![10, 5]);
    }

    #[test]
    fn compute_widths_caps_each_column_at_max_during_distribution() {
        // Wide enough that the proportional formula would overshoot col 0's max.
        let maxes = vec![vec![8, 8]];
        let mins = vec![vec![3, 3]];
        let widths = compute_widths(&maxes, &mins, 2, 23, None);
        assert_eq!(widths, vec![8, 8]);
    }

    #[test]
    fn compute_widths_respects_user_override() {
        let (maxes, mins) = maxes_eq_mins(vec![vec![1, 1, 1]]);
        let widths = compute_widths(&maxes, &mins, 3, 80, Some(&[Some(15), Some(7), Some(20)]));
        assert_eq!(widths, vec![15, 7, 20]);
    }

    #[test]
    fn compute_widths_clamps_user_override_to_min() {
        let (maxes, mins) = maxes_eq_mins(vec![vec![1, 1]]);
        let widths = compute_widths(&maxes, &mins, 2, 80, Some(&[Some(1), Some(0)])); // both below MIN
        assert_eq!(widths, vec![MIN_COL_WIDTH, MIN_COL_WIDTH]);
    }

    #[test]
    fn compute_widths_mixes_pinned_and_auto_columns() {
        let (maxes, mins) = maxes_eq_mins(vec![vec![3, 6]]);
        let widths = compute_widths(&maxes, &mins, 2, 80, Some(&[Some(5), None]));
        assert_eq!(widths, vec![5, 6]);
    }

    #[test]
    fn compute_widths_shrink_leaves_pinned_columns_alone() {
        // Pinned col 0 keeps its width; col 1 distributes from what's left, landing in
        // [4, 10].
        let maxes = vec![vec![10, 10]];
        let mins = vec![vec![10, 4]];
        let widths = compute_widths(&maxes, &mins, 2, 17, Some(&[Some(8), None]));
        assert_eq!(widths[0], 8); // pinned
        assert!(widths[1] >= MIN_COL_WIDTH);
        assert!(widths[1] <= 10);
    }

    #[test]
    fn compute_widths_narrow_prose_column_stays_at_min_with_no_slack() {
        // A viewport with no slack at all: the column gets exactly its min.
        let maxes = vec![vec![12]];
        let mins = vec![vec![3]];
        let widths = compute_widths(&maxes, &mins, 1, 7, None);
        assert_eq!(widths, vec![3]);
    }

    #[test]
    fn wrap_cell_breaks_on_spaces() {
        let rows = wrap_cell("hello world foo bar", 11);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].trim(), "hello world");
        assert_eq!(rows[1].trim(), "foo bar");
    }

    #[test]
    fn wrap_cell_empty_returns_one_empty_row() {
        let rows = wrap_cell("", 10);
        assert_eq!(rows, vec!["".to_owned()]);
    }

    #[test]
    fn wrap_cell_hard_splits_long_word() {
        let rows = wrap_cell("supercalifragilistic", 6);
        assert!(rows.len() >= 2);
        for r in &rows {
            assert!(UnicodeWidthStr::width(r.as_str()) <= 6);
        }
        let joined: String = rows.join("");
        assert_eq!(joined, "supercalifragilistic");
    }

    #[test]
    fn wrap_cell_hard_split_prefers_punctuation_break() {
        // A naive cut lands at char 10; the `_` at index 6 is in the trailing half, so
        // the cut moves just after it.
        let rows = wrap_cell("really_long_name", 10);
        assert_eq!(rows, vec!["really_", "long_name"]);
    }

    #[test]
    fn wrap_cell_hard_split_falls_back_to_full_width_without_punctuation() {
        let rows = wrap_cell("abcdefghijklmnop", 10);
        assert_eq!(rows, vec!["abcdefghij", "klmnop"]);
    }

    #[test]
    fn preferred_cut_ignores_punctuation_in_leading_half() {
        // Index 1 is outside the trailing-half scan window — full-width cut.
        let chunk: Vec<char> = "a_bcdefghi".chars().collect();
        assert_eq!(preferred_cut(chunk.len(), |i| chunk[i]), chunk.len());
    }

    #[test]
    fn parse_column_widths_comment_roundtrip() {
        let widths = vec![Some(20), Some(15), Some(30)];
        let s = format_column_widths_comment(&widths);
        assert_eq!(s, "<!-- tui-columns: [20, 15, 30] -->");
        let parsed = parse_column_widths_comment(&s).unwrap();
        assert_eq!(parsed, widths);
    }

    #[test]
    fn parse_column_widths_comment_handles_embedded_text() {
        let src = "some doc text\n<!-- tui-columns: [5, 7] -->\nrest of doc";
        let parsed = parse_column_widths_comment(src).unwrap();
        assert_eq!(parsed, vec![Some(5), Some(7)]);
    }

    #[test]
    fn parse_column_widths_comment_returns_none_when_absent() {
        assert!(parse_column_widths_comment("no widths here").is_none());
        assert!(parse_column_widths_comment("<!-- tui-columns: [a, b] -->").is_none());
    }

    #[test]
    fn parse_column_widths_comment_supports_auto_placeholders() {
        let parsed = parse_column_widths_comment("<!-- tui-columns: [10, _, 30] -->").unwrap();
        assert_eq!(parsed, vec![Some(10), None, Some(30)]);
    }

    #[test]
    fn parse_column_widths_comment_all_auto_returns_none() {
        assert!(parse_column_widths_comment("<!-- tui-columns: [_, _] -->").is_none());
    }

    #[test]
    fn format_column_widths_comment_emits_underscore_for_auto() {
        let widths = vec![Some(5), None, Some(12)];
        assert_eq!(
            format_column_widths_comment(&widths),
            "<!-- tui-columns: [5, _, 12] -->"
        );
    }

    // ── Pipe / cell helpers ────────────────────────────────────────────────

    use ratatui::text::{Line, Span};

    fn line_with(s: &str) -> Line<'static> {
        Line::from(vec![Span::raw(s.to_owned())])
    }

    #[test]
    fn raw_pipe_positions_basic() {
        assert_eq!(raw_pipe_positions("| a | b |"), vec![0, 4, 8]);
    }

    #[test]
    fn raw_pipe_positions_skips_escaped_pipes() {
        let row = r"| a \| x | b |";
        let pipes = raw_pipe_positions(row);
        assert_eq!(pipes, vec![0, 9, 13]);
    }

    #[test]
    fn rendered_pipe_positions_counts_box_drawing_pipes() {
        let line = line_with("│ a │ bb │");
        let pipes = rendered_pipe_positions(&line);
        assert_eq!(pipes, vec![0, 4, 9]);
    }

    #[test]
    fn table_raw_col_to_rendered_col_maps_first_cell() {
        // Both sides share a leading space, so raw col 2 ('a') maps to rendered col 2.
        let raw = "| a | b |";
        let rendered = line_with("│ a │ b │");
        assert_eq!(table_raw_col_to_rendered_col(raw, &rendered, 2), Some(1));
    }

    #[test]
    fn rendered_segments_map_continuation_chunk() {
        // Cell 1 wraps into ["alpha", "bravo"]; sub-line 1 shows "bravo" at raw 12..17.
        let raw = "| x | alpha bravo |";
        let rendered = line_with("│   │ bravo │");
        let segs = table_raw_col_range_to_rendered_segments(raw, &rendered, 13, 16, 1);
        assert_eq!(segs, vec![(7, 10)]);
        assert!(table_raw_col_range_to_rendered_segments(raw, &rendered, 2, 3, 1).is_empty());
        assert!(table_raw_col_range_to_rendered_segments(raw, &rendered, 13, 16, 2).is_empty());
    }

    #[test]
    fn rendered_segments_empty_on_pipe_mismatch() {
        let raw = "| a | b |";
        let rendered = line_with("├───┼───┤");
        assert!(table_raw_col_range_to_rendered_segments(raw, &rendered, 2, 3, 0).is_empty());
    }

    #[test]
    fn table_raw_col_to_rendered_col_returns_none_on_pipe_mismatch() {
        let raw = "| a | b |";
        let rendered = line_with("├───┼───┤");
        assert!(table_raw_col_to_rendered_col(raw, &rendered, 2).is_none());
    }

    #[test]
    fn compute_cell_overlay_none_when_raw_exceeds_rendered_width() {
        let raw = "| supercalifragilistic | b |";
        let rendered = line_with("│ a │ b │");
        assert!(compute_cell_overlay(raw, &rendered, 3).is_none());
    }

    #[test]
    fn compute_cell_overlay_returns_metadata_when_fits() {
        let raw = "| a | b |";
        let rendered = line_with("│ a │ b │");
        let overlay = compute_cell_overlay(raw, &rendered, 2).expect("overlay fits");
        assert_eq!(overlay.raw_text, " a ");
        assert_eq!(overlay.rendered_start, 1);
        assert_eq!(overlay.rendered_end, 4);
    }
}
