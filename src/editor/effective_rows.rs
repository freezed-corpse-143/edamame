//! Per-frame visual-row view with the raw-reveal patch applied.
//!
//! Part of paragraph reflow (`docs/dev/plans/paragraph-reflow.md`).  Once a paragraph
//! reflows, its rendered form can be *shorter* than its raw source (six wrapped-to-two rows),
//! so revealing the cursor's block as raw makes the document taller exactly while the cursor
//! rests inside it.  The old reveal is height-neutral and cannot express that.
//!
//! `EffectiveRows` presents the document's visual rows as if the revealed block's `N` rendered
//! lines were replaced by its `M` raw source lines, each wrapped at the current width — a cheap
//! delta over the base [`VisualRowCache`](crate::document::visual_cache::VisualRowCache), never a
//! rebuild.  When no block is revealed (Preview, the pre-reveal-delay window, or a non-reflowed
//! block whose reveal stays height-neutral), it is the identity over the base cache.
//!
//! The base cache clamps every wrapped row count to `>= 1`; the raw wrap counts here must too, or
//! a blank raw line reports zero rows and every prefix sum past it drifts.

use std::ops::Range;
use std::rc::Rc;

use crate::document::ParsedDoc;
use crate::ui::line_render::visual_rows_of_str;

/// What a visual row resolves to under the reveal patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowHit {
    /// A rendered line (index into `parsed.lines`) and its wrap sub-row.
    Rendered { line: usize, sub: usize },
    /// A raw source line of the revealed block — `raw_line` indexes the block's raw lines — and
    /// its wrap sub-row.
    Raw { raw_line: usize, sub: usize },
}

/// The reveal patch: the revealed block's rendered span and its raw lines' wrap geometry.
/// Opaque to callers (built by [`EffectiveRows::with_reveal`], shared via [`EffectiveRowsCache`]).
#[derive(Debug, Clone)]
pub struct Patch {
    /// Rendered-line range `[start, end)` of the revealed block.
    rendered: Range<usize>,
    /// Base visual rows in lines `[0, start)`.
    base_before: usize,
    /// Base visual rows in `[start, end)` — the rows the patch replaces.
    base_block_rows: usize,
    /// Wrap count (>= 1) of each raw line.
    raw_wrap: Vec<usize>,
    /// Prefix sums of `raw_wrap`; `len == raw_wrap.len() + 1`, last entry is the total.
    raw_prefix: Vec<usize>,
}

/// A per-frame view over one `ParsedDoc`'s visual rows.  Cheap to construct; holds only a
/// borrow plus a shared handle on the small patch.
///
/// The patch is `Rc`-shared so [`EditorState::effective_rows`](crate::editor::EditorState) can
/// cache it (see [`EffectiveRowsCache`]) and hand out many views per frame without re-allocating
/// the revealed block's source or recomputing its raw-line wrap counts — the queries here run
/// several times a frame (scroll, scrollbar, cursor row, gutter).
pub struct EffectiveRows<'a> {
    parsed: &'a ParsedDoc,
    width: usize,
    base_total: usize,
    patch: Option<Rc<Patch>>,
}

/// Per-`EditorState` memo for the reveal patch, so a frame's repeated `effective_rows` calls build
/// it once.  Keyed by `(parsed_version, width, revealed-block rendered-start)` — the only inputs
/// that change the patch; the revealed block's raw form depends on the block, not the cursor's
/// column within it, so intra-block cursor moves stay a cache hit.  `None` reveal start = identity.
#[derive(Debug, Clone, Default)]
pub struct EffectiveRowsCache {
    key: Option<(u64, usize, Option<usize>)>,
    base_total: usize,
    patch: Option<Rc<Patch>>,
}

impl EffectiveRowsCache {
    /// The cached patch when the key matches, else `None` (caller rebuilds).
    pub fn get(&self, key: (u64, usize, Option<usize>)) -> Option<(usize, Option<Rc<Patch>>)> {
        (self.key == Some(key)).then(|| (self.base_total, self.patch.clone()))
    }

    /// Store a freshly built patch under `key`.
    pub fn store(&mut self, key: (u64, usize, Option<usize>), base_total: usize, patch: Option<Rc<Patch>>) {
        self.key = Some(key);
        self.base_total = base_total;
        self.patch = patch;
    }
}

impl<'a> EffectiveRows<'a> {
    /// Identity view: every query delegates straight to the base cache.
    pub fn identity(parsed: &'a ParsedDoc, width: usize) -> Self {
        let width = width.max(1);
        Self {
            parsed,
            width,
            base_total: parsed.total_visual_rows(width),
            patch: None,
        }
    }

    /// View with the block at rendered range `rendered` revealed to `raw_lines`.  `raw_lines` are
    /// the block's raw source lines (soft breaks split back out); each is wrapped at `width`.
    pub fn with_reveal(
        parsed: &'a ParsedDoc,
        width: usize,
        rendered: Range<usize>,
        raw_lines: &[&str],
    ) -> Self {
        let width = width.max(1);
        Self {
            parsed,
            width,
            base_total: parsed.total_visual_rows(width),
            patch: Some(Rc::new(Patch::build(parsed, width, rendered, raw_lines))),
        }
    }

    /// Reconstruct a view from cached parts (see [`EffectiveRowsCache`]) — no allocation beyond an
    /// `Rc` clone.  `base_total` and `patch` must have been built at this same `width`.
    pub fn from_cached(
        parsed: &'a ParsedDoc,
        width: usize,
        base_total: usize,
        patch: Option<Rc<Patch>>,
    ) -> Self {
        Self {
            parsed,
            width: width.max(1),
            base_total,
            patch,
        }
    }

    /// Whether a reveal patch is in effect (i.e. this is not the identity view).
    pub fn has_reveal(&self) -> bool {
        self.patch.is_some()
    }

    /// The parts [`EffectiveRowsCache`] memoizes: the base total and a shared handle on the patch.
    pub fn cache_parts(&self) -> (usize, Option<Rc<Patch>>) {
        (self.base_total, self.patch.clone())
    }

    /// Rendered-line range of the revealed block, or `None` on the identity view.  The reveal
    /// loop uses `.start` to know where the raw-line unit is spliced in.
    pub fn block_rendered(&self) -> Option<Range<usize>> {
        self.patch.as_ref().map(|p| p.rendered.clone())
    }

    /// Wrap count (>= 1) of raw line `raw_line`, or 1 out of range / on the identity view.
    pub fn raw_wrap_at(&self, raw_line: usize) -> usize {
        self.patch
            .as_ref()
            .and_then(|p| p.raw_wrap.get(raw_line).copied())
            .unwrap_or(1)
    }

    /// Number of raw lines the revealed block expands to (0 on the identity view).
    pub fn raw_line_count(&self) -> usize {
        self.patch.as_ref().map_or(0, |p| p.raw_wrap.len())
    }

    /// Total visual rows with the patch applied.
    pub fn total_visual_rows(&self) -> usize {
        match &self.patch {
            None => self.base_total,
            Some(p) => self.base_total - p.base_block_rows + p.raw_rows_total(),
        }
    }

    /// Visual row a raw line of the revealed block starts on (absolute, document coordinates).
    /// Panics only if called on the identity view — callers gate on [`Self::has_reveal`].
    pub fn raw_line_visual_row(&self, raw_line: usize) -> usize {
        let p = self
            .patch
            .as_ref()
            .expect("raw_line_visual_row on identity view");
        p.base_before + p.raw_prefix[raw_line.min(p.raw_wrap.len())]
    }

    /// What visual row `v` resolves to under the patch.
    pub fn line_at_visual_row(&self, v: usize) -> RowHit {
        let Some(p) = &self.patch else {
            let (line, sub) = self.parsed.line_at_visual_row(v, self.width);
            return RowHit::Rendered { line, sub };
        };

        if v < p.base_before {
            // Before the block: base coordinates coincide.
            let (line, sub) = self.parsed.line_at_visual_row(v, self.width);
            return RowHit::Rendered { line, sub };
        }
        let raw_total = p.raw_rows_total();
        if v < p.base_before + raw_total {
            // Inside the revealed block: locate within the raw wrap prefix sums.
            let local = v - p.base_before;
            let raw_line = p
                .raw_prefix
                .partition_point(|&s| s <= local)
                .saturating_sub(1);
            let sub = local - p.raw_prefix[raw_line];
            return RowHit::Raw { raw_line, sub };
        }
        // After the block: shift back into base coordinates by the height delta.
        let base_coord = v + p.base_block_rows - raw_total;
        let (line, sub) = self.parsed.line_at_visual_row(base_coord, self.width);
        RowHit::Rendered { line, sub }
    }
}

impl Patch {
    /// Build the patch: measure each raw line's wrap at `width` and its prefix sums, plus the base
    /// rows the block spans (which the raw expansion replaces).
    fn build(parsed: &ParsedDoc, width: usize, rendered: Range<usize>, raw_lines: &[&str]) -> Self {
        let base_before = parsed.visual_rows_before(rendered.start, width);
        let base_end = parsed.visual_rows_before(rendered.end, width);
        let base_block_rows = base_end.saturating_sub(base_before);

        let mut raw_wrap = Vec::with_capacity(raw_lines.len());
        let mut raw_prefix = Vec::with_capacity(raw_lines.len() + 1);
        raw_prefix.push(0usize);
        let mut acc = 0usize;
        for line in raw_lines {
            let rows = visual_rows_of_str(line, width).len().max(1);
            raw_wrap.push(rows);
            acc += rows;
            raw_prefix.push(acc);
        }

        Self {
            rendered,
            base_before,
            base_block_rows,
            raw_wrap,
            raw_prefix,
        }
    }

    fn raw_rows_total(&self) -> usize {
        *self.raw_prefix.last().unwrap_or(&0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::Theme;
    use crate::document::ParsedDoc;
    use crate::ui::line_render::visual_rows_for_line;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    /// Build a reflowed parse (soft breaks joined) for a source string.
    fn reflowed(src: &str) -> ParsedDoc {
        ParsedDoc::build_with_overrides(
            src,
            theme(),
            false,
            24,
            None,
            None,
            false,
            80,
            false,
            false,
            true,
            true, // reflow on
            None,
        )
    }

    /// Brute-force expansion of the effective visual-row sequence: one `RowHit` per visual row.
    fn expand(
        parsed: &ParsedDoc,
        width: usize,
        reveal: Option<(Range<usize>, &[&str])>,
    ) -> Vec<RowHit> {
        let mut out = Vec::new();
        let n = parsed.lines.len();
        let (start, end, raw): (usize, usize, &[&str]) = match &reveal {
            Some((r, raw)) => (r.start, r.end, raw),
            None => (n, n, &[]),
        };
        for line in 0..n {
            if line == start && !raw.is_empty() {
                for (raw_line, text) in raw.iter().enumerate() {
                    let rows = visual_rows_of_str(text, width).len().max(1);
                    for sub in 0..rows {
                        out.push(RowHit::Raw { raw_line, sub });
                    }
                }
            }
            if line >= start && line < end {
                continue; // replaced by the raw lines above
            }
            let rows = visual_rows_for_line(&parsed.lines[line], width).max(1);
            for sub in 0..rows {
                out.push(RowHit::Rendered { line, sub });
            }
        }
        out
    }

    fn check_against_brute_force(
        parsed: &ParsedDoc,
        width: usize,
        reveal: Option<(Range<usize>, Vec<&str>)>,
    ) {
        let er = match &reveal {
            None => EffectiveRows::identity(parsed, width),
            Some((r, raw)) => EffectiveRows::with_reveal(parsed, width, r.clone(), raw),
        };
        let expected = expand(
            parsed,
            width,
            reveal.as_ref().map(|(r, raw)| (r.clone(), raw.as_slice())),
        );
        assert_eq!(
            er.total_visual_rows(),
            expected.len(),
            "total_visual_rows disagrees with brute force at width {width}"
        );
        for (v, want) in expected.iter().enumerate() {
            assert_eq!(
                er.line_at_visual_row(v),
                *want,
                "line_at_visual_row({v}) disagrees at width {width}"
            );
        }
    }

    #[test]
    fn identity_matches_base_at_several_widths() {
        let parsed = reflowed("# Title\n\nalpha beta gamma delta epsilon\n\nlast\n");
        for width in [80, 20, 12, 8] {
            check_against_brute_force(&parsed, width, None);
        }
    }

    #[test]
    fn reveal_taller_than_rendered_expands() {
        // A soft-broken paragraph: rendered as one (wrapping) line, revealed as 3 raw lines.
        let parsed = reflowed("intro\n\none\ntwo\nthree\n\nafter\n");
        // Locate the paragraph's rendered range: it renders "one two three" as a single line.
        let block = parsed
            .lines
            .iter()
            .position(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains("one two three")
            })
            .expect("reflowed paragraph must render as one line");
        let raw = vec!["one", "two", "three"];
        for width in [80, 20, 6] {
            check_against_brute_force(&parsed, width, Some((block..block + 1, raw.clone())));
        }
    }

    #[test]
    fn reveal_with_wrapping_raw_lines() {
        // Raw lines that themselves wrap at a narrow width.
        let parsed = reflowed("head\n\nalpha bravo\ncharlie delta echo\n\ntail\n");
        let block = parsed
            .lines
            .iter()
            .position(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains("alpha bravo charlie")
            })
            .expect("reflowed paragraph must render as one line");
        let raw = vec!["alpha bravo", "charlie delta echo"];
        for width in [80, 10, 6] {
            check_against_brute_force(&parsed, width, Some((block..block + 1, raw.clone())));
        }
    }

    #[test]
    fn raw_line_visual_row_matches_expansion() {
        let parsed = reflowed("intro\n\none\ntwo\nthree\n\nafter\n");
        let block = parsed
            .lines
            .iter()
            .position(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains("one two three")
            })
            .unwrap();
        let raw = vec!["one", "two", "three"];
        let width = 8;
        let er = EffectiveRows::with_reveal(&parsed, width, block..block + 1, &raw);
        let expected = expand(&parsed, width, Some((block..block + 1, &raw)));
        // The first visual row of each raw line must be a `Raw { sub: 0 }` at the reported row.
        for raw_line in 0..raw.len() {
            let vr = er.raw_line_visual_row(raw_line);
            assert_eq!(expected[vr], RowHit::Raw { raw_line, sub: 0 });
        }
    }
}
