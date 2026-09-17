# Inline math as an image on the text's own row — the atom design

Status: **IMPLEMENTED (spike, 2026-09-17).** Open as PR #55. This record exists so the shortcuts
below are deliberate and each one has a named upgrade path. Sibling context:
[`media-export.md`](../media-export.md) (the block image pipeline this consumes),
[`editing-model.md`](../editing-model.md) (the wrap and raw-reveal invariants it must not break),
[`../../terminal-compatibility.md`](../../terminal-compatibility.md) (which terminals can place it).

## Goal

Render `$…$` inside a sentence as a formula image that occupies the formula's **measured ink** on the
text's own row, and show the source again for the row the cursor is on. The question the spike had to
answer on a real terminal: does a one-cell-row-tall image placed inside a text row read as inline
math, and what does the layout side cost?

## Decision

- **Width == ink, not source length.** `$Y_1$` is five source characters and about three cells of
  ink; reserving the source's width left a visible gap after every short formula. The atom is emitted
  as exactly the measured number of **single-width, unbreakable** characters, so wrap, `source_map`,
  the cursor column and mouse mapping are untouched.
- **The atom is found by a sentinel, not by re-finding `$…$`** in the rendered text (private-use code
  points carrying the atom's ordinal), so the painter never needs the source's width.
- **One more consumer of the existing block image pipeline** (RaTeX → resvg → cache → decode worker →
  paint), not a second pipeline. `paint` walks the visible rows, finds each sentinel, and paints one
  erase sweep, one transmit (first frame), and one short placement escape per frame in which the atom
  moved.
- **Reveal = lay the formula out as its source text again** for the row holding the cursor; the row
  re-wraps by the ink/source difference, exactly as the block-level reveal re-flows its rows.
  `EditorState` answers *which* formula (via `set_build_inputs`); the renderer only asks
  (`is_revealed(ordinal, source)`).
- **The bitmap is flattened** onto the document background instead of left transparent: the atom's
  cells carry `code_span` styling and the placement's erase sweep refills them with the background
  *colour*, so a transparent formula would show that chip through its own empty pixels. Display math
  flattens for the same reason.
- **Two terminal paths, chosen from the environment.** WezTerm → kitty direct placement (`a=p`);
  Windows Terminal → Sixel (no image ids; text written over the cells erases the image, so the reveal
  *is* the rewrite). `EDAMAME_INLINE_MATH` overrides when set (`0`/`off` disables); unset, the
  terminal decides and every other terminal stays **inert** (literal source, as before).
  WezTerm's markers win over an inherited `WT_SESSION` — a WezTerm started from a Windows Terminal tab
  carries both, and the sixel path would render nothing there.

## Consequences and deliberate shortcuts

| Shortcut | Why it is acceptable as a spike | Upgrade path |
|---|---|---|
| Atom table, build inputs and bitmap cache are process-global (`thread_local`) | fine for a handful of formulas | thread them through `ParsedDoc::build` and the decode worker |
| Formulas are measured on the render path (one RaTeX parse + layout per distinct formula, cached) | a keystroke in a math-heavy document pays once per formula | measure in the decode worker |
| Width is measured at one cell size, probed once per session | a mid-session font change is rare | invalidate the layout on a cell-size change |
| Placement is cell-aligned; the sub-cell `Y` offset is wired but left 0 | the padded raster answers the baseline question first | enable `Y` where a font needs sub-cell precision |
| Only direct-placement terminals (WezTerm, Windows Terminal) | they are the two that can place inside a row today | a `U=1` placeholder emitter for kitty/Ghostty; iTerm2 stays open |

## Consent: the atom path joins the *Figures* gate

An atom rasterizes on the render path — its bitmap cache is a side effect of rendering — so unlike a
block figure it never passes through the decode dispatch, which is the only place
`config.figures.enabled` is otherwise enforced. A session that set Figures to **Never** therefore
still got pictures, while [`../../editing.md`](../../editing.md) promised math shares that prompt.

The fix keeps the answer in one place and pushes it along the path that already exists:

- `BuildInputs` carries a `consent` flag. `cell_size()` returns `None` without it — so the renderer's
  `Inline::Math` arm takes its literal-source path — and `is_painting()` is the predicate shared by
  the painter and `render_cache::is_cache_worthy`, because a declined session must keep its table's
  memoization.
- `EditorState::figures_consent` carries the *paint* answer, deliberately not the layout one: an
  unanswered `Ask` reserves rows but must draw nothing. It defaults to `true` like the sibling media
  flags and `app::configure_new_editor` narrows it from `effective_diagrams_enabled` — the same
  permissive-default-then-narrow shape the other flags use.
- `dispatch_image_decodes_for` mirrors that answer into the editor **before** its early return, so a
  decline (which dispatches nothing) still reaches the atoms; the inequality guard is what keeps the
  re-parse from recursing.
- Pinned by `a_declined_figures_setting_never_reaches_the_paint_path`, plus the cache test's new
  "declined consent is cache-worthy again" assertion.

## Measured on real terminals

WezTerm, 100×32, cell 14×32 px: formula capital top 1268 against the text's capitals 1268 —
pixel-identical baselines; `$Y_1$` occupies ~3 cells of ink versus 5 source characters, and the gap
before the following comma drops from ~2 cells to sub-cell.

Windows Terminal (nominal 10×20 px sixel grid against a real 14×32 cell): baseline exact (formula ink
bottom 225–226 against the label's own ink bottom 225); writing `XXXX` over an atom removes its ink,
and a single `X` inside a 3-cell atom leaves a column-scoped gap with both neighbours intact; two
atoms on one row survive at x=319 and x=389, the five cells apart they were placed. Sixel's own 6-px
row means a clipped atom can sit up to one band off the cell grid — the one thing the kitty path does
better.

## Rejected alternatives

- **Split the source across cells / change the column map.** Breaks the raw↔rendered 1:1 invariant
  every mapping consumer depends on; the atom being ordinary text is what made this shippable.
- **Reserve the source's width and centre the ink.** Leaves the visible gap the first cut had.
- **Render it as a block image between rows.** Not inline math; it changes the paragraph's shape.

## Testing

24 new `#[test]`s: `src/image/inline_math.rs` (atom span and its trailing sentinel, scan order, ink
width, cursor-row reveal and its rebuild rule, sixel erase, last-row literal, WezTerm-marker
precedence, wide glyph before a formula, fallback to source, revealed raw row, scroll re-placement,
refused atom, cell payload carries the escape and no glyphs), `src/image/sixel.rs` (payload shape and
band breaks, box fully painted, nothing below the declared height, ink-free bitmap, baseline row),
`src/diagram/math.rs` (ink factor, body-size setting, ink bottom on the baseline),
`src/markdown/render_cache.rs` (a math table is cache-worthy again while the spike is off) and
`src/ui/line_render.rs` (an atom run breaks only after its sentinel).
