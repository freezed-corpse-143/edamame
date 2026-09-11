//! Per-width visual-row prefix-sum cache shared by rendered mode (`ParsedDoc`) and raw mode
//! (`EditorState`). Without it every scroll event re-wraps every line (O(N)), which a fast
//! trackpad swipe turns into visible lag. Invalidated by a width change; callers with extra
//! staleness keys (e.g. buffer version) wrap it in their own check.

/// Per-line wrapped row counts plus their prefix sum, so scroll arithmetic is O(1) and
/// visual-row → line lookup is O(log N).
#[derive(Debug, Clone)]
pub(crate) struct VisualRowCache {
    /// Viewport width this cache was built for.
    pub(crate) width: usize,
    /// Wrapped row count of each line at `width`, always at least 1.
    pub(crate) visual_rows_per_line: Vec<usize>,
    /// `[i]` = sum of `visual_rows_per_line[0..i]`; one entry longer than the line count, so
    /// the last entry is the total.
    pub(crate) visual_row_prefix_sum: Vec<usize>,
}

impl VisualRowCache {
    /// Build by calling `rows_for(idx)` for each line; counts are clamped to at least 1 to match
    /// the renderer's treatment of blank lines.
    pub(crate) fn build<F>(line_count: usize, width: usize, mut rows_for: F) -> Self
    where
        F: FnMut(usize) -> usize,
    {
        let mut per_line = Vec::with_capacity(line_count);
        let mut prefix = Vec::with_capacity(line_count + 1);
        prefix.push(0usize);
        let mut acc = 0usize;
        for idx in 0..line_count {
            let rows = rows_for(idx).max(1);
            per_line.push(rows);
            acc = acc.saturating_add(rows);
            prefix.push(acc);
        }
        Self {
            width,
            visual_rows_per_line: per_line,
            visual_row_prefix_sum: prefix,
        }
    }

    pub(crate) fn width(&self) -> usize {
        self.width
    }

    /// Visual rows occupied by line `idx`; 1 for out-of-range indices.
    pub(crate) fn for_line(&self, idx: usize) -> usize {
        self.visual_rows_per_line.get(idx).copied().unwrap_or(1)
    }

    /// Visual rows occupied by lines `[0..idx)`; saturates at the total.
    pub(crate) fn before(&self, idx: usize) -> usize {
        let clamped = idx.min(self.visual_rows_per_line.len());
        self.visual_row_prefix_sum
            .get(clamped)
            .copied()
            .unwrap_or(0)
    }

    /// Visual rows occupied by lines `[first..=last]`; 0 for an empty cache or reversed range.
    #[allow(dead_code)]
    pub(crate) fn between(&self, first: usize, last: usize) -> usize {
        if first > last || self.visual_rows_per_line.is_empty() {
            return 0;
        }
        let last = last.min(self.visual_rows_per_line.len() - 1);
        self.before(last + 1).saturating_sub(self.before(first))
    }

    /// Total visual rows across all lines.
    pub(crate) fn total(&self) -> usize {
        self.before(self.visual_rows_per_line.len())
    }

    /// `(line_idx, sub_row)` for a document-level visual row; `(line_count, 0)` past the end so
    /// callers can stop rendering without special-case arithmetic.
    pub(crate) fn find_visual_row(&self, visual_row: usize) -> (usize, usize) {
        let total = self.total();
        if visual_row >= total {
            return (self.visual_rows_per_line.len(), 0);
        }
        // Smallest `i` with `prefix[i+1] > visual_row`; the prefix array has one extra entry.
        let target = visual_row + 1;
        let upper = self.visual_row_prefix_sum.partition_point(|&p| p < target);
        let line = upper.saturating_sub(1);
        let start = self.visual_row_prefix_sum[line];
        (line, visual_row - start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cache_reports_zero_total() {
        let cache = VisualRowCache::build(0, 80, |_| 1);
        assert_eq!(cache.total(), 0);
        assert_eq!(cache.before(0), 0);
        assert_eq!(cache.find_visual_row(0), (0, 0));
    }

    #[test]
    fn prefix_sum_matches_per_line_counts() {
        let counts = [1usize, 2, 1, 3, 1];
        let cache = VisualRowCache::build(counts.len(), 80, |i| counts[i]);
        assert_eq!(cache.total(), 8);
        assert_eq!(cache.before(0), 0);
        assert_eq!(cache.before(1), 1);
        assert_eq!(cache.before(3), 4);
        assert_eq!(cache.before(5), 8);
        assert_eq!(cache.between(1, 3), 2 + 1 + 3);
        assert_eq!(cache.for_line(2), 1);
    }

    #[test]
    fn find_visual_row_lands_on_correct_line_and_subrow() {
        let counts = [2usize, 1, 3];
        let cache = VisualRowCache::build(counts.len(), 80, |i| counts[i]);
        assert_eq!(cache.find_visual_row(0), (0, 0));
        assert_eq!(cache.find_visual_row(1), (0, 1));
        assert_eq!(cache.find_visual_row(2), (1, 0));
        assert_eq!(cache.find_visual_row(3), (2, 0));
        assert_eq!(cache.find_visual_row(4), (2, 1));
        assert_eq!(cache.find_visual_row(5), (2, 2));
        assert_eq!(cache.find_visual_row(6), (3, 0));
        assert_eq!(cache.find_visual_row(100), (3, 0));
    }

    #[test]
    fn rows_for_zero_is_clamped_to_one() {
        let cache = VisualRowCache::build(2, 80, |_| 0);
        assert_eq!(cache.for_line(0), 1);
        assert_eq!(cache.total(), 2);
    }
}
