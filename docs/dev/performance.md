# Performance — the parse/render pipeline

Contributor-facing reference for the one hot path in edamame: the eager, full-document work an edit triggers. It records the budget that path is held to, what each stage costs, why the two optimizations in place exist, and which ceilings are known and deliberately unfixed.

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

`nested` is the guard for the render cache's skip-cheap-blocks rule: its `Table`/`CodeBlock` blocks live inside a `List`/`BlockQuote`, so its cold→memoized gap collapses if a future change stops caching containers of expensive content. Its cold cost is high by design (highlighting + table measurement on every unit).

Two details of the harness matter for reproducibility:

- **Grammars are warmed on the bench thread first** (`warm_grammars`, calling `highlight::warm_inline`). Highlighting is eventually-consistent in the live app — a cold grammar renders plain while a background worker compiles it — so without the warm call the `code` and `mixed` numbers would be a coin-toss mixture of the highlighted and plain paths.
- **`full_pipeline_memoized` alternates between two source variants differing in one character**, so every build is a warm cache with exactly one changed block. That is the steady-state edit cost; `full_pipeline` is the cold-open / paste-whole-document cost.
- **`build_doc` runs with paragraph reflow on**, matching the shipped `reflow = true` default; `render_only` sets the same flag so the derived `other` residual stays honest.

`cargo bench --bench pipeline` to reproduce.

## Results

Two reference machines, run with identical bench configuration (release profile, criterion sample size 10, reflow on, all seven mixes). The **shapes** — which stage dominates, how each mix scales — hold across both; the **absolute numbers** do not. The Linux box measures roughly **2–2.5× slower than the M3** (5k `prose` steady-state: 4.3 → 10.7 ms), so compare within a machine, never divide one machine's figure by the other's. All times are criterion means from one run.

The analysis after each pair of tables (dominant stage, where memoization helps, linear scaling) describes machine-independent shape and is written once, against the M3 numbers.

### Apple M3 (macOS)

Run 2026-09-10 on an Apple M3 (8 cores, macOS 15.7.5), rustc 1.98.0, criterion 0.8.

#### Steady-state edit — `full_pipeline_memoized`

What a line-crossing keystroke costs in the live editor: warm `RenderCache`, one block changed.

| Corpus | 1k lines | 5k | 20k | 100k |
|---|---|---|---|---|
| `prose` | 0.89 ms | 4.34 ms | 20.4 ms | 107.3 ms |
| `lists` | 0.75 ms | 3.65 ms | 16.3 ms | 86.8 ms |
| `tables` | 1.01 ms | 5.43 ms | 24.8 ms | 134.3 ms |
| `code` | 0.29 ms | 1.04 ms | 4.33 ms | 29.4 ms |
| `math` | 0.48 ms | 2.21 ms | 9.70 ms | 51.9 ms |
| `nested` | 0.49 ms | 2.27 ms | 11.0 ms | 70.9 ms |
| `mixed` | 0.66 ms | 3.13 ms | 14.0 ms | 74.8 ms |

Against the budget: **every mix is inside one frame at 5k lines**, and `mixed` still is at 20k (14.0 ms — marginal, past the 8 ms working target). At 20k `prose`, `lists` and `tables` exceed a frame; at 100k every mix does. Scaling is linear throughout — no stage is accidentally quadratic.

#### Cold open — `full_pipeline`

Opening a document, or pasting one wholesale: no cache, everything rendered.

| Corpus | 1k lines | 5k | 20k | 100k |
|---|---|---|---|---|
| `prose` | 0.81 ms | 4.31 ms | 20.4 ms | 107.7 ms |
| `lists` | 0.69 ms | 3.58 ms | 16.0 ms | 83.7 ms |
| `tables` | 3.39 ms | 17.8 ms | 74.6 ms | 382.4 ms |
| `code` | 4.01 ms | 20.1 ms | 81.3 ms | 413.1 ms |
| `math` | 0.49 ms | 2.26 ms | 9.56 ms | 52.3 ms |
| `nested` | 6.40 ms | 33.4 ms | 135.0 ms | 691.2 ms |
| `mixed` | 1.38 ms | 6.69 ms | 28.9 ms | 150.6 ms |

No keystroke waits on a cold open, so the frame budget does not apply to this table the way it does to the one above — but this is what a whole-document paste costs, and it is the number to watch when adding work to the renderer.

#### Stage breakdown at 20k lines

`other` = `full − (parse_merged + render_only)`: post-passes, virtual blank-line blocks, `SourceMap`, anchors. `parse_offsets` and `parse_ast` are the pre-merge baselines, kept for comparison only — neither runs in the pipeline any more. Small residuals (`code`'s is slightly negative) are measurement noise.

| Corpus | full | `parse_merged` | `render_only` | other | dominant | (`parse_offsets` / `parse_ast`) |
|---|---|---|---|---|---|---|
| `prose` | 20.4 ms | 12.8 ms | 5.16 ms | 2.45 ms | **parse 63%** | 4.39 / 12.7 ms |
| `lists` | 16.0 ms | 9.20 ms | 4.60 ms | 2.19 ms | **parse 58%** | 2.86 / 9.01 ms |
| `tables` | 74.6 ms | 14.6 ms | 57.4 ms | 2.70 ms | **render 77%** | 4.26 / 14.0 ms |
| `code` | 81.3 ms | 0.45 ms | 81.5 ms | ~0 | **render ~100%** | 0.31 / 0.43 ms |
| `math` | 9.56 ms | 2.73 ms | 1.76 ms | 5.06 ms | **other 53%** | 1.51 / 2.63 ms |
| `nested` | 135.0 ms | 4.62 ms | 127.9 ms | 2.52 ms | **render 95%** | 1.55 / 4.52 ms |
| `mixed` | 28.9 ms | 7.49 ms | 19.7 ms | 1.78 ms | **render 68%** | 2.30 / 7.00 ms |

`mixed` stage scaling across 1k / 5k / 20k / 100k is linear: `parse_merged` 0.35 / 1.78 / 7.49 / 40.4 ms, `render_only` 0.94 / 4.63 / 19.7 / 100.9 ms.

Two shapes worth naming:

- **`code` is the most expensive corpus to render cold.** Its parse is nearly free (0.45 ms — a fenced block is one AST node), so syntax highlighting is essentially the entire pipeline there. It is also the corpus the render cache helps most (81.3 → 4.33 ms at 20k), because a code block's AST is unchanged by edits to other blocks and the highlighter is never re-entered for it. `nested` (a `rust` fence wrapped in a list item) inherits the same profile at higher absolute cost.
- **`math` is the one `other`-bound corpus.** Parse and render are both small; the residual is the per-formula `$$` source scan, image-block promotion, and source-map / anchor derivation over many short blocks — the inverse of every other mix, which is parse- or render-bound. It stays comfortably inside budget.

#### Where memoization helps, and where it doesn't

Change in the 20k figure, `full_pipeline` → `full_pipeline_memoized`:

| Corpus | Change |
|---|---|
| `code` | −95% |
| `nested` | −92% |
| `tables` | −67% |
| `mixed` | −52% |
| `prose` | ~0% |
| `lists` | +2% |
| `math` | +2% |

The cache pays off on the expensive-to-render mixes — `code`, `nested`, `tables`, `mixed` — where a re-render costs far more than a hash-and-clone lookup. The cheap-to-render mixes — `prose`, `lists`, `math` — no longer enter the cache at all (`render_cache::is_cache_worthy` skips them), so their memoized cost simply tracks their cold cost instead of paying hash-plus-clone for nothing. `nested` is the proof the gate walks *into* containers: its expensive `rust` fence and table stay cached even though the outer block is a cheap list/blockquote, so it drops from 135 ms cold to 11 ms memoized. See [the two optimizations](#the-two-optimizations-and-why-they-must-not-be-undone) and the clone-on-hit ceiling.

#### Resize — `visual_cache_build`

Cold prefix-sum rebuild on the `mixed` corpus: 1.37 / 6.68 / 24.7 / 114.5 ms at 1k / 5k / 20k / 100k. Over one frame from roughly 20k lines, but it fires only on a width change and is already behind the 80 ms `RESIZE_QUIESCE` window — leave it alone unless live resize jank shows up.

### Intel Core Ultra 7 258V (Linux)

Run 2026-09-10 on an Intel Core Ultra 7 258V (8 cores, Linux 6.16 / Debian 13), rustc 1.98.0, criterion 0.8. Same shapes as the M3, ~2–2.5× slower in absolute terms. Against the 16 ms frame budget on *this* box, steady-state edits stay inside a frame only up to ~1k lines for the heavy mixes and cross it by 5–20k — a slower-hardware statement, not a regression.

#### Steady-state edit — `full_pipeline_memoized`

| Corpus | 1k | 5k | 20k | 100k |
|---|---|---|---|---|
| `prose` | 1.97 ms | 10.7 ms | 47.3 ms | 267.2 ms |
| `lists` | 1.54 ms | 8.35 ms | 40.3 ms | 215.0 ms |
| `tables` | 2.83 ms | 15.3 ms | 75.5 ms | 399.4 ms |
| `code` | 1.00 ms | 4.06 ms | 22.4 ms | 172.1 ms |
| `math` | 1.07 ms | 5.80 ms | 25.3 ms | 138.8 ms |
| `nested` | 1.34 ms | 7.79 ms | 49.3 ms | 289.4 ms |
| `mixed` | 1.56 ms | 7.86 ms | 39.8 ms | 215.9 ms |

#### Cold open — `full_pipeline`

| Corpus | 1k | 5k | 20k | 100k |
|---|---|---|---|---|
| `prose` | 1.67 ms | 9.61 ms | 45.2 ms | 283.3 ms |
| `lists` | 1.47 ms | 7.89 ms | 39.8 ms | 203.7 ms |
| `tables` | 6.32 ms | 35.8 ms | 153.6 ms | 762.7 ms |
| `code` | 8.52 ms | 41.4 ms | 167.2 ms | 845.4 ms |
| `math` | 1.06 ms | 5.67 ms | 24.6 ms | 134.5 ms |
| `nested` | 12.9 ms | 67.7 ms | 289.9 ms | 1443 ms |
| `mixed` | 2.94 ms | 15.0 ms | 67.7 ms | 344.9 ms |

#### Stage breakdown at 20k lines

| Corpus | full | `parse_merged` | `render_only` | other | dominant | (`parse_offsets` / `parse_ast`) |
|---|---|---|---|---|---|---|
| `prose` | 45.2 ms | 31.3 ms | 12.4 ms | 1.5 ms | **parse 69%** | 10.2 / 29.1 ms |
| `lists` | 39.8 ms | 23.7 ms | 11.9 ms | 4.2 ms | **parse 60%** | 7.6 / 21.6 ms |
| `tables` | 153.6 ms | 40.0 ms | 108.9 ms | 4.7 ms | **render 71%** | 11.4 / 36.9 ms |
| `code` | 167.2 ms | 1.2 ms | 162.4 ms | ~3.5 ms | **render ~97%** | 0.7 / 1.1 ms |
| `math` | 24.6 ms | 7.4 ms | 3.6 ms | 13.6 ms | **other 55%** | 3.9 / 7.0 ms |
| `mixed` | 67.7 ms | 19.1 ms | 43.4 ms | 5.3 ms | **render 64%** | 6.4 / 17.6 ms |

`mixed` stage scaling across 1k / 5k / 20k / 100k stays linear: `parse_merged` 0.78 / 4.39 / 19.1 / 109.8 ms, `render_only` 1.90 / 10.1 / 43.4 / 233.2 ms.

#### Resize — `visual_cache_build`

Cold prefix-sum rebuild on the `mixed` corpus: 3.02 / 14.5 / 61.7 / 296.1 ms at 1k / 5k / 20k / 100k. Same shape as the M3 row, ~2.5× slower. Fires only on a width change and sits behind the 80 ms `RESIZE_QUIESCE` window, so it is one rebuild per quiesced drag, not per frame — left alone.

The `visual_cache_build` group uses **flat sampling** (`SamplingMode::Flat`, 5 s measurement time), not criterion's default linear sampling. A single rebuild at 100k lines is ~0.3 s — larger than a 2 s window can fit more than one iteration into per sample, which makes linear sampling misreport the mean wildly. Flat sampling runs a fixed iteration count per sample and reports slow routines correctly; the numbers above are stable across repeats. The group's width-cycling still forces a genuine cold rebuild every call.

## The two optimizations, and why they must not be undone

- **One parse, not two.** The pipeline used to parse the document twice — once for byte offsets, once for the AST. `parse_raw_with_ranges` collects the ranges from a `parse_offsets::RangeTracker` observing the same offset-iterator events the AST builder consumes, so blocks and ranges stay 1:1 *by construction* rather than by a second pass agreeing with the first. Re-splitting them costs a full extra parse per reparse.
- **Block-level render memoization.** `RenderCache` (owned by `EditorState`, threaded into every `refresh_parsed`) keys rendered lines by the `Block` AST value plus a render-settings fingerprint, so an unchanged block costs a clone of its lines instead of a re-render. This is what makes table-heavy documents editable at all — table column measurement dominates everything else. Keying by AST rather than source bytes is what keeps live table-width drags and post-pass promotions correct. Two properties of the cache earn their keep:
  - The map hashes `Block` keys with `FxHasher` (`rustc-hash`), not std's SipHash. The keys are local document content, so SipHash's DoS resistance buys nothing, and its cost on the many small `write_*` calls a nested `Block` makes is pure overhead. (seahash, already in the tree, benches *slower* than SipHash here — it is tuned for whole byte buffers, not the small-write struct hashing a `Block` key does; hence `rustc-hash`.)
  - Only *cache-worthy* blocks are stored (`render_cache::is_cache_worthy`): a `Table`/`CodeBlock`, or a `List`/`BlockQuote` containing one. Cheap blocks (paragraphs, plain lists, headings, rules) re-render for less than a lookup costs, so caching them is a net loss. The gate is a subtree walk, not a match on the outer kind — that is what keeps an expensive block wrapped in a cheap container (the `nested` mix) cached instead of re-rendering every keystroke.

Both claims are asserted, not just documented:

- `merged_parse_matches_two_pass_parse` (`src/markdown/parser.rs`) pins the merged parse to the old two-pass pairing.
- `cached_render_matches_uncached`, plus the eviction, settings-invalidation, syntax-toggle and image-bypass tests (`src/markdown/renderer.rs`), pin cached rendering to uncached output. `cheap_blocks_bypass_cache` and `is_cache_worthy_follows_nested_expensive_content` pin the cache-worthy gate — including that a list/blockquote wrapping a `Table`/`CodeBlock` stays cached.

## Known ceilings

These are recorded as facts about the current design, not as tasks.

- **The full-document parse floor.** The single parse is O(document) and cannot be memoized the way rendering is — 7.5 ms at 20k `mixed`, 40 ms at 100k (M3). It is the dominant cost for prose and lists and the floor under everything else. Only region-limited / incremental reparsing removes it — reparsing the edited block and its neighbors, with care around fences, setext headings, lists and footnote definitions, all of which have non-local effects. That is a separate project, explicitly out of scope here.
- **Clone-on-hit.** A cache hit hashes the `Block` to find its entry and then clones its `Vec<Line>` into the output. The `FxHasher` key hash and the cache-worthy gate (above) keep this from ever being a net loss: cheap blocks skip the cache entirely, so they no longer pay hash-plus-clone for nothing, and on the blocks that *are* cached (`tables`, `code`, nested containers) the clone is a small share of their large render. What remains is that clone — a haircut on the win, not a loss. Removing it means sharing lines as `Arc<[Line]>`, which changes `ParsedDoc::lines`' type and ripples through every view; worth it only if very large table-/code-heavy documents matter in practice.
- **Resize.** `visual_cache_build` is the cold prefix-sum rebuild a width change forces (24.7 ms at 20k `mixed`, M3). It exceeds a frame on large documents, but fires only on resize and sits behind the 80 ms `RESIZE_QUIESCE` window (`app::frame_timer`), so it is left alone.
- **`parse_offsets::top_level_block_ranges` is off the edit path entirely.** It survives as the pre-merge baseline the bench measures and as the oracle in `merged_parse_matches_two_pass_parse`; the diff subsystem uses the sibling `block_ranges_by`, not this. Its cost is not an editing cost.

## When to re-measure

Re-run the benches and update the tables above when changing anything on the pipeline: a new render pass or block kind, a change to table layout or the inline renderer, a new `RenderSettings` field (which invalidates the whole cache when it changes), or a change to how highlighting is parsed or capped. Note the machine — these numbers are only comparable within one.
