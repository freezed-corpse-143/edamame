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

`benches/pipeline.rs` generates its documents in-process — deterministic, no on-disk corpus — at **1k / 5k / 20k / 100k source lines** in seven mixes. The mixes exist because cost per block varies enormously; keep them stable, since they are what makes a future measurement comparable to the ones below.

| Corpus | Composition | Stresses |
|---|---|---|
| `prose` | Paragraphs with bold/links | Inline rendering, virtual blank-line blocks, reflow |
| `lists` | Deep nested lists with checkboxes | List post-pass and rendering |
| `tables` | Many medium tables | Table column measurement (`table_layout`) |
| `code` | Fenced `rust` blocks | Syntax highlighting, NBSP padding, cheap inlines |
| `math` | Prose with inline `$…$` + stacked `$$…$$` blocks | Math delimiter scan, per-formula display-math promotion/split |
| `nested` | Lists wrapping `rust` code, blockquotes wrapping tables | The cache's subtree-gate (`is_cache_worthy`): expensive content inside a cheap container must stay cached |
| `mixed` | Blend + headings + footnotes | Anchors, source map, everything |

`math` measures only the *synchronous* parse + promotion work: a `$$…$$` block promotes to a `Block::ImageBlock` that reserves space, and the RaTeX raster behind it is produced later by the async decode worker — off this path, like every image and mermaid diagram.

`nested` is the guard for the render cache's skip-cheap-blocks rule (#35 §2): its `Table`/`CodeBlock` blocks live inside a `List`/`BlockQuote`, so its cold→memoized gap collapses if a future change stops caching containers of expensive content. Its cold cost is high by design (highlighting + table measurement on every unit).

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

`lists` is a reproducible regression, not noise (confirmed on a repeat run). The corpus's blocks are large nested-list ASTs that are cheap to render — 4.71 ms for the whole document — but expensive to *look up*, since the cache hashes the entire `Block` value on every query and then clones the cached lines back out. Where re-rendering costs less than hashing plus cloning, the cache is a net loss. It stays because the mixes that resemble real documents — `mixed`, `tables`, `code` — gain far more than `lists` loses, and no real document is wall-to-wall deep nested lists. (This regression was later fixed: #35 §2 skips caching cheap blocks, so `lists` no longer enters the cache — see the second baseline and the clone-on-hit ceiling. This M3 row predates that.)

### Resize — `visual_cache_build`

Cold prefix-sum rebuild on the `mixed` corpus: 1.42 / 7.01 / 23.9 / 138.3 ms at 1k / 5k / 20k / 100k. Over one frame from roughly 20k lines, but it fires only on a width change and is already behind the 80 ms `RESIZE_QUIESCE` window — leave it alone unless live resize jank shows up.

## Second baseline — reflow + display math

Run 2026-09-10 on an Intel Core Ultra 7 258V (8 cores, Linux 6.16 / Debian 13), rustc 1.98.0, release profile, criterion 0.8, sample size 10. This is the first run with the shipped defaults after the two features: `build_doc` runs **reflow on**, and a `math` mix is present.

This machine measures roughly **2–2.5× slower than the M3** above (5k `prose` steady-state: 3.85 → 10.2 ms), so it is a *separate* baseline, not a delta on the M3 tables. Compare shapes within each block; do not divide one machine's number by the other's. Against the 16 ms frame budget on *this* box, steady-state edits stay inside a frame only up to ~1k lines for the heavy mixes; every mix crosses it by 5–20k. That is a slower-hardware statement, not a regression — no stage is quadratic, and scaling is linear throughout.

The **steady-state table below was refreshed after the two render-cache changes in #35** (§1 FxHasher, §2 skip-caching cheap blocks; see "The two optimizations" and the clone-on-hit ceiling). Cold open, stage breakdown, and resize are unchanged by those — the cold path builds no cache — so those tables are the original reflow+math run. The `nested` mix and its rows were added with §2.

### Steady-state edit — `full_pipeline_memoized`

Post-#35 §1+§2. `lists` and `math` no longer regress against their cold cost (both now skip the cache); `nested` proves the cache still serves expensive content wrapped in a list/blockquote (memoized ≈ 6× below its cold open).

| Corpus | 1k | 5k | 20k | 100k |
|---|---|---|---|---|
| `prose` | 1.97 ms | 10.7 ms | 47.3 ms | 267.2 ms |
| `lists` | 1.54 ms | 8.35 ms | 40.3 ms | 215.0 ms |
| `tables` | 2.83 ms | 15.3 ms | 75.5 ms | 399.4 ms |
| `code` | 1.00 ms | 4.06 ms | 22.4 ms | 172.1 ms |
| `math` | 1.07 ms | 5.80 ms | 25.3 ms | 138.8 ms |
| `nested` | 1.34 ms | 7.79 ms | 49.3 ms | 289.4 ms |
| `mixed` | 1.56 ms | 7.86 ms | 39.8 ms | 215.9 ms |

### Cold open — `full_pipeline`

| Corpus | 1k | 5k | 20k | 100k |
|---|---|---|---|---|
| `prose` | 1.67 ms | 9.61 ms | 45.2 ms | 283.3 ms |
| `lists` | 1.47 ms | 7.89 ms | 39.8 ms | 203.7 ms |
| `tables` | 6.32 ms | 35.8 ms | 153.6 ms | 762.7 ms |
| `code` | 8.52 ms | 41.4 ms | 167.2 ms | 845.4 ms |
| `math` | 1.06 ms | 5.67 ms | 24.6 ms | 134.5 ms |
| `nested` | 12.9 ms | 67.7 ms | 289.9 ms | 1443 ms |
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
- **`math` used to show the `lists`-style memoization regression** (+13% memoized vs cold at 20k on the original run, since its blocks are cheap-to-render placeholders). #35 §2 (skip caching cheap blocks) removed it: `math` and `lists` now bypass the cache, so their memoized cost tracks cold (`math` 25.3 vs 24.6; `lists` 40.3 vs 39.8 at 20k) instead of exceeding it. See the clone-on-hit ceiling below.

### Resize — `visual_cache_build`

Cold prefix-sum rebuild on the `mixed` corpus: 3.02 / 14.5 / 61.7 / 296.1 ms at 1k / 5k / 20k / 100k. Same shape as the M3 row, ~2.5× slower. Fires only on a width change and sits behind the 80 ms `RESIZE_QUIESCE` window, so it is one rebuild per quiesced drag, not per frame — left alone.

The `visual_cache_build` group now uses **flat sampling** (`SamplingMode::Flat`, 5 s measurement time). A single rebuild at 100k lines is ~0.3 s — larger than the group's old 2 s window, so the default linear sampling could only fit one iteration per sample and misreported the mean (nanoseconds one run, ±42% in [#35](https://github.com/mijowi/edamame/issues/35)). Flat sampling runs a fixed iteration count per sample and reports slow routines correctly; the numbers above are stable across repeats. This is a harness fix, not a cache change — the cache's width-cycling still forces a genuine cold rebuild every call.

## The two optimizations, and why they must not be undone

- **One parse, not two.** The pipeline used to parse the document twice — once for byte offsets, once for the AST. `parse_raw_with_ranges` collects the ranges from a `parse_offsets::RangeTracker` observing the same offset-iterator events the AST builder consumes, so blocks and ranges stay 1:1 *by construction* rather than by a second pass agreeing with the first. Re-splitting them costs a full extra parse per reparse.
- **Block-level render memoization.** `RenderCache` (owned by `EditorState`, threaded into every `refresh_parsed`) keys rendered lines by the `Block` AST value plus a render-settings fingerprint, so an unchanged block costs a clone of its lines instead of a re-render. This is what makes table-heavy documents editable at all — table column measurement dominated everything else. Keying by AST rather than source bytes is what keeps live table-width drags and post-pass promotions correct. Two refinements from #35: the map hashes `Block` keys with `FxHasher` (`rustc-hash`) rather than std's SipHash — the keys are local document content, so SipHash's DoS resistance buys nothing and its cost on the many small `write_*` calls a nested `Block` makes was pure overhead (§1) — and only *cache-worthy* blocks are stored (`render_cache::is_cache_worthy`): a `Table`/`CodeBlock`, or a `List`/`BlockQuote` containing one. Cheap blocks re-render for less than a lookup costs, so caching them was a net loss (§2). The subtree walk is what keeps expensive content wrapped in a list from silently re-rendering every keystroke.

Both claims are asserted, not just documented:

- `merged_parse_matches_two_pass_parse` (`src/markdown/parser.rs`) pins the merged parse to the old two-pass pairing.
- `cached_render_matches_uncached`, plus the eviction, settings-invalidation, syntax-toggle and image-bypass tests (`src/markdown/renderer.rs`), pin cached rendering to uncached output. `cheap_blocks_bypass_cache` and `is_cache_worthy_follows_nested_expensive_content` pin the §2 gate — including that a list/blockquote wrapping a `Table`/`CodeBlock` stays cached.

## Known ceilings

These are recorded as facts about the current design, not as tasks.

- **The full-document parse floor.** The single parse is still O(document) and cannot be memoized the way rendering is — 7.7 ms at 20k `mixed`, 43.2 ms at 100k. It is the dominant cost for prose and lists and the floor under everything else. Only region-limited / incremental reparsing removes it — reparsing the edited block and its neighbors, with care around fences, setext headings, lists and footnote definitions, all of which have non-local effects. That is a separate project, explicitly out of scope here.
- **Clone-on-hit.** A cache hit hashes the `Block` to find its entry and then clones its `Vec<Line>` into the output. Both #35 refinements landed, and between them the old "cache is a net loss on cheap blocks" problem is gone:
  - **§1 — `FxHasher` for the key hash.** Re-running `full_pipeline_memoized` after moving off SipHash cut ~3–12% (typically ~6%) off every mix, largest where hashing was the largest share. (seahash, already in the tree, was tried first and benched *slower* than SipHash — it is tuned for whole byte buffers, not the small-write struct hashing a `Block` key does; hence `rustc-hash`.)
  - **§2 — skip caching cheap blocks.** Blocks that render for less than a lookup costs (paragraphs, plain lists, headings, rules) no longer enter the cache, so they stop paying hash+clone for nothing. `lists` and `math` — which used to be net losses — now track their cold cost (`lists` 40.3 vs 39.8, `math` 25.3 vs 24.6 at 20k) instead of exceeding it, and improved outright at 100k (`lists` 347 → 215 ms). The gate walks into `List`/`BlockQuote` so a wrapped `Table`/`CodeBlock` stays cached (the `nested` mix guards this: memoized 49 ms vs cold 290 ms at 20k).
  - **What remains:** the clone itself, on the blocks that *are* cached (`tables`, `code`, nested containers). It is a smaller share of their large render, so it is no longer a net loss anywhere — only a haircut on the win. Removing it means sharing lines as `Arc<[Line]>`, which changes `ParsedDoc::lines`' type and ripples through every view; worth it only if very large table-/code-heavy documents matter in practice.
- **Resize.** `visual_cache_build` is the cold prefix-sum rebuild a width change forces (23.9 ms at 20k `mixed`). It exceeds a frame on large documents, but fires only on resize and sits behind the 80 ms `RESIZE_QUIESCE` window (`app::frame_timer`), so it is left alone.
- **`parse_offsets::top_level_block_ranges` is off the edit path entirely.** It survives as the pre-merge baseline the bench measures and as the oracle in `merged_parse_matches_two_pass_parse`; the diff subsystem uses the sibling `block_ranges_by`, not this. Its cost is not an editing cost.

## When to re-measure

Re-run the benches and update the tables above when changing anything on the pipeline: a new render pass or block kind, a change to table layout or the inline renderer, a new `RenderSettings` field (which invalidates the whole cache when it changes), or a change to how highlighting is parsed or capped. Note the machine — these numbers are only comparable within one.
