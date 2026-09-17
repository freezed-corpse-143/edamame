//! Block-level render memoization for the parse → render pipeline; see
//! docs/dev/performance.md.
//!
//! [`RenderCache`] keys each block's rendered lines by the block's AST *value*, not by its
//! source bytes: everything that changes rendering without changing source text (table-width
//! drag overrides, post-pass promotions, list splitting) mutates the AST, so such a block
//! simply misses the cache.
//!
//! `Block::ImageBlock` is never cached — its row count depends on the image decode cache,
//! which changes out-of-band as decodes complete, and re-rendering a placeholder is free.

use std::collections::HashMap;

use ratatui::text::Line;
use rustc_hash::FxBuildHasher;

use super::ast::{Block, Inline};

/// The cache's block map. Keyed by whole `Block` AST values, so a lookup hashes a deep
/// structure — many small `write_*` calls — on every query. std's DoS-resistant SipHash is
/// both wasted (the keys are local document content, never adversarial network input) and slow
/// at that write pattern, which is what makes the cache a net loss on cheap-to-render blocks
/// (`lists`, `math`; see docs/dev/performance.md and issue #35). `FxHasher` is built for small
/// struct keys and measurably faster here; the AST keying — hence correctness — is unchanged.
/// (seahash, the crate's other non-crypto hasher, is for whole byte buffers and benched *slower*
/// than SipHash on these keys.)
pub(super) type BlockMap = HashMap<Block, Vec<Line<'static>>, FxBuildHasher>;

/// Whether a block is worth memoizing. A cache hit costs a hash of the whole `Block` plus a clone
/// of its `Vec<Line>`; only blocks whose render is *more* expensive than that come out ahead. That
/// is `Table` (column measurement) and `CodeBlock` (syntax highlighting) — and any `List` or
/// `BlockQuote` that *contains* one, since skipping those would re-run the expensive nested render
/// on every keystroke. Cheap blocks (paragraphs, plain lists, headings, rules) render for less than
/// a lookup costs, so caching them is a net loss (#35 §2; see docs/dev/performance.md); they bypass
/// the cache and re-render each build. `ImageBlock` is handled separately by the caller — it is
/// never cached for an unrelated reason (its rows track the out-of-band decode cache).
pub(super) fn is_cache_worthy(block: &Block) -> bool {
    // A block whose render registers inline-math atoms must re-run every build: the atom table is
    // a side effect of *rendering* (see `image::inline_math`), so a cache hit would leave a hole in
    // it and shift every later formula's ordinal — the painter would then name the wrong atom.
    //
    // Only while the spike is actually *painting*, though.  With it off — an unsupported terminal,
    // `EDAMAME_INLINE_MATH=0`, or a session that declined the *Figures* consent — those inlines
    // render as plain text with no side effect, and bypassing the cache for them is a pure
    // regression for readers of documents that merely mention `$`.
    if crate::image::inline_math::is_painting() && has_inline_math(block) {
        return false;
    }
    match block {
        Block::Table { .. } | Block::CodeBlock { .. } => true,
        Block::BlockQuote { blocks } => blocks.iter().any(is_cache_worthy),
        Block::List { items, .. } => items.iter().flat_map(|it| &it.blocks).any(is_cache_worthy),
        _ => false,
    }
}

/// Whether `block` contains an inline `$…$` anywhere it would be rendered.
fn has_inline_math(block: &Block) -> bool {
    let inlines = |v: &Vec<Inline>| v.iter().any(|i| matches!(i, Inline::Math { .. }));
    match block {
        Block::Paragraph { inlines: i } | Block::Heading { inlines: i, .. } => inlines(i),
        Block::Table { headers, rows, .. } => {
            headers.iter().any(inlines) || rows.iter().flatten().any(inlines)
        }
        Block::BlockQuote { blocks } | Block::FootnoteDefinition { blocks, .. } => {
            blocks.iter().any(has_inline_math)
        }
        Block::List { items, .. } => items.iter().flat_map(|it| &it.blocks).any(has_inline_math),
        _ => false,
    }
}

/// Fingerprint of every `Renderer` input besides the block itself; any change clears the whole
/// cache.  The theme is identified by address — themes are `&'static` and the editor already
/// treats pointer identity as theme identity (`EditorState::set_theme` uses `ptr::eq`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenderSettings {
    pub theme_addr: usize,
    pub viewport_width: usize,
    pub code_wrap: bool,
    pub image_max_height: usize,
    pub row_striping: bool,
    pub big_h1: bool,
    pub syntax_highlighting: bool,
    /// Reflow prose paragraphs (soft breaks → spaces, wrap to viewport).  A render-output knob, so
    /// it belongs in the fingerprint: toggling it (e.g. a mode switch into Raw) must clear the cache.
    pub reflow_paragraphs: bool,
    /// `highlight::warm_generation()` at build time, or 0 when highlighting is off.  Warming a
    /// grammar changes a code block's rendered lines without changing its `Block` value, so
    /// without this the block would stay uncolored for the life of the document.
    pub highlight_generation: u64,
    /// `highlight::retry_epoch()` at build time, or 0 when highlighting is off.  Covers the case
    /// the counter above cannot: a grammar the burst budget turned away.  A retry is granted
    /// exactly when no grammar warmed, so without this the retry's reparse hits every cached
    /// block, never reaches `render_code_block`, and never re-asks for the slot it was granted.
    pub highlight_retry_epoch: u64,
}

/// Memoized rendered lines per top-level block, owned by `EditorState` and threaded into
/// `ParsedDoc::build_with_overrides` on every reparse.  Eviction is by document membership:
/// each build moves the entries it hits into a fresh map and drops the old one.
#[derive(Debug, Default)]
pub struct RenderCache {
    pub(super) settings: Option<RenderSettings>,
    pub(super) entries: BlockMap,
}

impl RenderCache {
    /// Reset to the given settings, clearing all entries when they differ from the previous
    /// build's.  Returns the previous entry map; the caller moves hits out of it into the fresh
    /// [`entries`](Self::entries) map and lets the remainder drop.
    pub(super) fn begin_build(&mut self, settings: RenderSettings) -> BlockMap {
        if self.settings.as_ref() != Some(&settings) {
            self.entries.clear();
            self.settings = Some(settings);
        }
        std::mem::take(&mut self.entries)
    }
}

#[cfg(test)]
mod tests {

    /// F5: the bypass in `is_cache_worthy` exists because rendering *registers* atoms — a side
    /// effect that only happens while the spike is painting.  With it off (an unsupported terminal,
    /// `EDAMAME_INLINE_MATH=0`, or a session that declined *Figures*) a table that merely mentions
    /// `$` must keep its cache entry; the unconditional bypass cost those readers the whole table's
    /// memoization.
    #[test]
    fn a_table_with_math_is_cache_worthy_again_while_the_spike_is_off() {
        let blocks = crate::markdown::parser::parse("| $x$ | b |\n|---|---|\n| 1 | 2 |\n");
        let table = blocks.first().expect("a table block");
        crate::image::inline_math::force_enabled(false);
        assert!(
            is_cache_worthy(table),
            "no atom side effect while the spike is off, so caching is safe"
        );
        crate::image::inline_math::force_enabled(true);
        crate::image::inline_math::set_build_inputs(Some((10, 20)), None, false);
        assert!(
            is_cache_worthy(table),
            "a declined *Figures* consent registers no atoms either"
        );
        crate::image::inline_math::set_build_inputs(Some((10, 20)), None, true);
        assert!(
            !is_cache_worthy(table),
            "the atom table is a side effect of rendering, so a cache hit would shift the ordinals"
        );
    }
    use super::*;

    fn settings() -> RenderSettings {
        RenderSettings {
            theme_addr: 0,
            viewport_width: 80,
            code_wrap: false,
            image_max_height: 20,
            row_striping: false,
            big_h1: false,
            syntax_highlighting: true,
            reflow_paragraphs: false,
            highlight_generation: 0,
            highlight_retry_epoch: 0,
        }
    }

    /// The retry epoch's absence is invisible: it moves exactly when `highlight_generation` does
    /// *not*, so a cache ignoring it keeps serving the plain render of a budget-refused block.
    #[test]
    fn both_highlight_counters_clear_the_cache() {
        for bump in [
            |s: &mut RenderSettings| s.highlight_generation += 1,
            |s: &mut RenderSettings| s.highlight_retry_epoch += 1,
        ] {
            let mut cache = RenderCache::default();
            cache.begin_build(settings());
            cache
                .entries
                .insert(Block::HorizontalRule, vec![Line::from("x")]);

            let prev = cache.begin_build(settings());
            assert_eq!(prev.len(), 1, "an unchanged fingerprint must not clear");
            cache.entries = prev;

            let mut changed = settings();
            bump(&mut changed);
            let prev = cache.begin_build(changed);
            assert!(prev.is_empty(), "a moved counter must clear the cache");
        }
    }
}
