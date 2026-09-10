# Performance — the parse/render pipeline

Contributor-facing reference for the one hot path in edamame: the eager, full-document work an edit triggers. It records the budget that path is held to, what each stage costs, why the two optimizations in place exist, and which ceilings are known and deliberately unfixed.

The history — the branch A–E decision tree these numbers were originally gathered to choose between, and the pre-optimization measurements — is in [`plans/archive/perf-benchmark-plan.md`](plans/archive/perf-benchmark-plan.md). This page carries only what is still true.

## What runs on an edit

Every line-crossing edit — and every deferred flush of an in-line typing burst — calls `EditorState::refresh_parsed()`, which rebuilds the whole document:

1. **Parse.** One pulldown-cmark pass (`markdown::parser::parse_raw_with_ranges`) yielding the AST *and* top-level byte ranges together.
2. **Post-passes.** List blank annotation, image/diagram/comment promotion.
3. **Render.** Every top-level block to styled `Vec<Line<'static>>` (`Renderer::render_with_counts_cached`), memoized per block.
4. **Derive.** `SourceMap`, heading/footnote anchors, blank-line virtual blocks.

Separately, a width change rebuilds the `VisualRowCache` prefix sum over all lines (`ParsedDoc::ensure_visual_rows`).

The draw layer is *not* on this list: it is viewport-limited in every mode (Preview, Rendered, Raw, Diff), so document size does not enter it.

## The budget

The frame throttle is 16 ms (`app::frame_timer::MIN_FRAME_INTERVAL`, ~60 fps). For editing to feel jank-free, a line-crossing keystroke must finish `refresh_parsed()` *and* one draw inside one interval:

| `refresh_parsed()` | Verdict |
|---|---|
| ≤ 8 ms | fine — leaves headroom for the draw |
| 8–16 ms | marginal; optimize the dominant stage |
| > 16 ms | visible jank on every Enter keypress |

## The corpus

`benches/pipeline.rs` generates its documents in-process — deterministic, no on-disk corpus — at **1k / 5k / 20k / 100k source lines** in six mixes. The mixes exist because cost per block varies enormously; keep them stable, since they are what makes a future measurement comparable to the ones below.

| Corpus | Composition | Stresses |
|---|---|---|
| `prose` | Paragraphs with bold/links | Inline rendering, virtual blank-line blocks, reflow |
| `lists` | Deep nested lists with checkboxes | List post-pass and rendering |
| `tables` | Many medium tables | Table column measurement (`table_layout`) |
| `code` | Fenced `rust` blocks | Syntax highlighting, NBSP padding, cheap inlines |
| `math` | Prose with inline `$…$` + stacked `$$…$$` blocks | Math delimiter scan, per-formula display-math promotion/split |
| `mixed` | Blend + headings + footnotes | Anchors, source map, everything |

`math` measures only the *synchronous* parse + promotion work: a `$$…$$` block promotes to a `Block::ImageBlock` that reserves space, and the RaTeX raster behind it is produced later by the async decode worker — off this path, like every image and mermaid diagram.

Two details of the harness matter for reproducibility:

- **Grammars are warmed on the bench thread first** (`warm_grammars`, calling `highlight::warm_inline`). Highlighting is eventually-consistent in the live app — a cold grammar renders plain while a background worker compiles it — so without the warm call the `code` and `mixed` numbers would be a coin-toss mixture of the highlighted and plain paths.
- **`full_pipeline_memoized` alternates between two source variants differing in one character**, so every build is a warm cache with exactly one changed block. That is the steady-state edit cost; `full_pipeline` is the cold-open / paste-whole-document cost.
- **`build_doc` runs with paragraph reflow on**, matching the shipped `reflow = true` default; `render_only` sets the same flag so the derived `other` residual stays honest. The M3 tables above predate the flag and ran with it off.

`cargo bench --bench pipeline` to reproduce.

## Results

Run 2026-08-22 on an Apple M3 (8 cores, macOS 15.7.5), rustc 1.96.1, release profile, criterion 0.5, sample size 10. Times are criterion means, all from one run. These supersede the 2026-06-10 figures in the archived plan, which predate syntax highlighting and were taken on different hardware — compare shapes, not ratios, across the two.

This run **predates paragraph reflow and display math**: `build_doc` ran with reflow off, and there was no `math` mix. The [second baseline](#second-baseline--reflow--display-math) below covers both on a different machine. Reflow is neutral-to-slightly-cheaper on this path (see there), so the shapes below still hold; the `math` profile is new.

### Steady-state edit — `full_pipeline_memoized`

What a line-crossing keystroke costs in the live editor: warm `RenderCache`, one block changed.

| Corpus | 1k lines | 5k | 20k | 100k |
|---|---|---|---|---|
| `prose` | 0.82 ms | 3.85 ms | 18.4 ms | 98.0 ms |
| `lists` | 0.97 ms | 5.05 ms | 23.0 ms | 122.7 ms |
| `tables` | 1.06 ms | 5.76 ms | 27.6 ms | 145.5 ms |
| `code` | 0.34 ms | 1.17 ms | 4.77 ms | 32.1 ms |
| `mixed` | 0.67 ms | 3.15 ms | 13.7 ms | 81.1 ms |

Against the budget: **every mix is inside one frame at 5k lines**, and `mixed` still is at 20k (13.7 ms — marginal, past the 8 ms working target). At 20k `prose`, `lists` and `tables` exceed a frame; at 100k every mix does. Scaling is linear throughout — no stage is accidentally quadratic.

### Cold open — `full_pipeline`

Opening a document, or pasting one wholesale: no cache, everything rendered.

| Corpus | 1k lines | 5k | 20k | 100k |
|---|---|---|---|---|
| `prose` | 0.84 ms | 4.31 ms | 20.5 ms | 105.5 ms |
| `lists` | 0.76 ms | 3.81 ms | 16.0 ms | 88.5 ms |
| `tables` | 3.46 ms | 18.4 ms | 76.0 ms | 389.2 ms |
| `code` | 4.03 ms | 20.4 ms | 82.6 ms | 415.5 ms |
| `mixed` | 1.41 ms | 6.83 ms | 29.8 ms | 154.3 ms |

No keystroke waits on a cold open, so the frame budget does not apply to this table the way it does to the one above — but this is what a whole-document paste costs, and it is the number to watch when adding work to the renderer.

### Stage breakdown at 20k lines

`other` = `full − (parse_merged + render_only)`: post-passes, virtual blank-line blocks, `SourceMap`, anchors. `parse_offsets` and `parse_ast` are the pre-merge baselines, kept for comparison only — neither runs in the pipeline any more. Small residuals (`code`'s is slightly negative) are measurement noise.

| Corpus | full | `parse_merged` | `render_only` | other | dominant | (`parse_offsets` / `parse_ast`) |
|---|---|---|---|---|---|---|
| `prose` | 20.5 ms | 13.0 ms | 5.47 ms | 2.03 ms | **parse 63%** | 4.56 / 12.7 ms |
| `lists` | 16.0 ms | 9.37 ms | 4.71 ms | 1.96 ms | **parse 58%** | 2.96 / 9.03 ms |
| `tables` | 76.0 ms | 14.9 ms | 59.3 ms | 1.72 ms | **render 78%** | 4.42 / 13.8 ms |
| `code` | 82.6 ms | 0.49 ms | 82.9 ms | ~0 | **render ~100%** | 0.32 / 0.44 ms |
| `mixed` | 29.8 ms | 7.66 ms | 20.1 ms | 2.03 ms | **render 67%** | 2.86 / 7.24 ms |

`mixed` stage scaling across 1k / 5k / 20k / 100k is linear: `parse_merged` 0.35 / 1.82 / 7.66 / 43.2 ms, `render_only` 0.94 / 4.77 / 20.1 / 103.5 ms.

Two things changed shape since the pre-highlighting measurements:

- **`code` is now the most expensive corpus to render cold**, where it used to be the cheapest mix in the table. Its parse is nearly free (0.49 ms — a fenced block is one AST node), so syntax highlighting is essentially the entire pipeline there. It is also the corpus the render cache helps most (82.6 → 4.77 ms at 20k), because a code block's AST is unchanged by edits to other blocks and the highlighter is never re-entered for it.
- **`tables` is no longer the single worst case**, though table column measurement is still the second-largest line item and the worst *steady-state* one.

### Where memoization helps, and where it doesn't

Change in the 20k figure, `full_pipeline` → `full_pipeline_memoized`:

| Corpus | Change |
|---|---|
| `code` | −94% |
| `tables` | −64% |
| `mixed` | −54% |
| `prose` | −10% |
| `lists` | **+44%** |

`lists` is a reproducible regression, not noise (confirmed on a repeat run). The corpus's blocks are large nested-list ASTs that are cheap to render — 4.71 ms for the whole document — but expensive to *look up*, since the cache hashes the entire `Block` value on every query and then clones the cached lines back out. Where re-rendering costs less than hashing plus cloning, the cache is a net loss. It stays because the mixes that resemble real documents — `mixed`, `tables`, `code` — gain far more than `lists` loses, and no real document is wall-to-wall deep nested lists. If it is ever revisited, the fix is the one the clone-on-hit ceiling below names.

### Resize — `visual_cache_build`

Cold prefix-sum rebuild on the `mixed` corpus: 1.42 / 7.01 / 23.9 / 138.3 ms at 1k / 5k / 20k / 100k. Over one frame from roughly 20k lines, but it fires only on a width change and is already behind the 80 ms `RESIZE_QUIESCE` window — leave it alone unless live resize jank shows up.

## Second baseline — reflow + display math

Run 2026-09-10 on an Intel Core Ultra 7 258V (8 cores, Linux 6.16 / Debian 13), rustc 1.98.0, release profile, criterion 0.8, sample size 10. This is the first run with the shipped defaults after the two features: `build_doc` runs **reflow on**, and a `math` mix is present.

This machine measures roughly **2–2.5× slower than the M3** above (5k `prose` steady-state: 3.85 → 10.2 ms), so it is a *separate* baseline, not a delta on the M3 tables. Compare shapes within each block; do not divide one machine's number by the other's. Against the 16 ms frame budget on *this* box, steady-state edits stay inside a frame only up to ~1k lines for the heavy mixes; every mix crosses it by 5–20k. That is a slower-hardware statement, not a regression — no stage is quadratic, and scaling is linear throughout.

### Steady-state edit — `full_pipeline_memoized`

| Corpus | 1k | 5k | 20k | 100k |
|---|---|---|---|---|
| `prose` | 1.94 ms | 10.2 ms | 44.6 ms | 252.9 ms |
| `lists` | 2.35 ms | 12.8 ms | 64.6 ms | 347.4 ms |
| `tables` | 2.92 ms | 16.2 ms | 73.4 ms | 389.0 ms |
| `code` | 1.01 ms | 4.24 ms | 24.3 ms | 168.9 ms |
| `math` | 1.09 ms | 6.33 ms | 27.8 ms | 163.9 ms |
| `mixed` | 1.61 ms | 8.22 ms | 43.3 ms | 232.4 ms |

### Cold open — `full_pipeline`

| Corpus | 1k | 5k | 20k | 100k |
|---|---|---|---|---|
| `prose` | 1.67 ms | 9.61 ms | 45.2 ms | 283.3 ms |
| `lists` | 1.47 ms | 7.89 ms | 39.8 ms | 203.7 ms |
| `tables` | 6.32 ms | 35.8 ms | 153.6 ms | 762.7 ms |
| `code` | 8.52 ms | 41.4 ms | 167.2 ms | 845.4 ms |
| `math` | 1.06 ms | 5.67 ms | 24.6 ms | 134.5 ms |
| `mixed` | 2.94 ms | 15.0 ms | 67.7 ms | 344.9 ms |

### Stage breakdown at 20k lines

| Corpus | full | `parse_merged` | `render_only` | other | dominant | (`parse_offsets` / `parse_ast`) |
|---|---|---|---|---|---|---|
| `prose` | 45.2 ms | 31.3 ms | 12.4 ms | 1.5 ms | **parse 69%** | 10.2 / 29.1 ms |
| `lists` | 39.8 ms | 23.7 ms | 11.9 ms | 4.2 ms | **parse 60%** | 7.6 / 21.6 ms |
| `tables` | 153.6 ms | 40.0 ms | 108.9 ms | 4.7 ms | **render 71%** | 11.4 / 36.9 ms |
| `code` | 167.2 ms | 1.2 ms | 162.4 ms | ~3.5 ms | **render ~97%** | 0.7 / 1.1 ms |
| `math` | 24.6 ms | 7.4 ms | 3.6 ms | 13.6 ms | **other 55%** | 3.9 / 7.0 ms |
| `mixed` | 67.7 ms | 19.1 ms | 43.4 ms | 5.3 ms | **render 64%** | 6.4 / 17.6 ms |

`mixed` stage scaling across 1k / 5k / 20k / 100k stays linear: `parse_merged` 0.78 / 4.39 / 19.1 / 109.8 ms, `render_only` 1.90 / 10.1 / 43.4 / 233.2 ms.

### What the two features cost

- **Reflow is neutral-to-slightly-cheaper on the pipeline.** A controlled same-machine on/off run (cold `full_pipeline`) put reflow *on* at 9.61 ms vs *off* at 10.3 ms for 20k `prose`, and 344.9 vs 369.0 ms for 100k `mixed` — a ~6–9% *win* on prose, flat elsewhere. Reflow emits one `Line` per top-level paragraph instead of one per source line, so the pipeline allocates fewer lines and caches fewer entries; the wrap itself is paid at draw time, which is viewport-limited and off this path. The reveal-aware `EffectiveRows` overlay is likewise viewport arithmetic, not `refresh_parsed` work, so nothing there is on the measured path.
- **`math` is cheap but `other`-dominated.** Parse and render are both small (7.4 / 3.6 ms at 20k); the 55% `other` residual is the per-formula `$$` source scan, image-block promotion, and source-map / anchor derivation over many short blocks — the inverse of every other corpus, which is parse- or render-bound. It stays comfortably inside budget.
- **`math` shows the `lists`-style memoization regression** (+13% memoized vs cold at 20k; 24.6 → 27.8 ms). Its blocks are cheap-to-render placeholders, so the cache's hash-whole-`Block` + clone-lines-out costs more than re-rendering — the clone-on-hit ceiling below, not a new problem.

### Resize — `visual_cache_build`

Cold prefix-sum rebuild on the `mixed` corpus: 3.02 / 14.5 / 61.7 / 296.1 ms at 1k / 5k / 20k / 100k. Same shape as the M3 row, ~2.5× slower. Fires only on a width change and sits behind the 80 ms `RESIZE_QUIESCE` window, so it is one rebuild per quiesced drag, not per frame — left alone.

The `visual_cache_build` group now uses **flat sampling** (`SamplingMode::Flat`, 5 s measurement time). A single rebuild at 100k lines is ~0.3 s — larger than the group's old 2 s window, so the default linear sampling could only fit one iteration per sample and misreported the mean (nanoseconds one run, ±42% in [#35](https://github.com/mijowi/edamame/issues/35)). Flat sampling runs a fixed iteration count per sample and reports slow routines correctly; the numbers above are stable across repeats. This is a harness fix, not a cache change — the cache's width-cycling still forces a genuine cold rebuild every call.

## The two optimizations, and why they must not be undone

- **One parse, not two.** The pipeline used to parse the document twice — once for byte offsets, once for the AST. `parse_raw_with_ranges` collects the ranges from a `parse_offsets::RangeTracker` observing the same offset-iterator events the AST builder consumes, so blocks and ranges stay 1:1 *by construction* rather than by a second pass agreeing with the first. Re-splitting them costs a full extra parse per reparse.
- **Block-level render memoization.** `RenderCache` (owned by `EditorState`, threaded into every `refresh_parsed`) keys rendered lines by the `Block` AST value plus a render-settings fingerprint, so an unchanged block costs a clone of its lines instead of a re-render. This is what makes table-heavy documents editable at all — table column measurement dominated everything else. Keying by AST rather than source bytes is what keeps live table-width drags and post-pass promotions correct.

Both claims are asserted, not just documented:

- `merged_parse_matches_two_pass_parse` (`src/markdown/parser.rs`) pins the merged parse to the old two-pass pairing.
- `cached_render_matches_uncached`, plus the eviction, settings-invalidation, syntax-toggle and image-bypass tests (`src/markdown/renderer.rs`), pin cached rendering to uncached output.

## Known ceilings

These are recorded as facts about the current design, not as tasks.

- **The full-document parse floor.** The single parse is still O(document) and cannot be memoized the way rendering is — 7.7 ms at 20k `mixed`, 43.2 ms at 100k. It is the dominant cost for prose and lists and the floor under everything else. Only region-limited / incremental reparsing removes it — reparsing the edited block and its neighbors, with care around fences, setext headings, lists and footnote definitions, all of which have non-local effects. That is a separate project, explicitly out of scope here.
- **Clone-on-hit, and hash-on-lookup.** A cache hit hashes the whole `Block` to find its entry and then clones its `Vec<Line>` into the output — together about 4.0 ms of the 13.7 ms steady-state cost at 20k `mixed`, and more than the entire render it replaces on `lists`. Sharing lines as `Arc<[Line]>` (and/or keying on a cheaper block identity) would change `ParsedDoc::lines`' type and ripple through every view; worth it only if very large or list-heavy documents matter in practice.
- **Resize.** `visual_cache_build` is the cold prefix-sum rebuild a width change forces (23.9 ms at 20k `mixed`). It exceeds a frame on large documents, but fires only on resize and sits behind the 80 ms `RESIZE_QUIESCE` window (`app::frame_timer`), so it is left alone.
- **`parse_offsets::top_level_block_ranges` is off the edit path entirely.** It survives as the pre-merge baseline the bench measures and as the oracle in `merged_parse_matches_two_pass_parse`; the diff subsystem uses the sibling `block_ranges_by`, not this. Its cost is not an editing cost.

## When to re-measure

Re-run the benches and update the tables above when changing anything on the pipeline: a new render pass or block kind, a change to table layout or the inline renderer, a new `RenderSettings` field (which invalidates the whole cache when it changes), or a change to how highlighting is parsed or capped. Note the machine — these numbers are only comparable within one.
