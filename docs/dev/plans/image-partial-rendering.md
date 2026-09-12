# Partial image rendering — the visible band as the interface

Branch: `image-partial-rendering` (this branch implements **M1: the Kitty backend**)
Rebased onto `713baaf` (`main`, v0.1.4). All line references below are against that commit.
Issue: [mijowi/edamame#50](https://github.com/mijowi/edamame/issues/50)

## Problem

`paint_images` (`src/ui/image_view.rs:303`) gates the native graphics protocol on
the image's **full reserved rect** fitting inside the viewport (`:320,348`):

```rust
let fully_visible = top >= viewport_top && bottom <= viewport_bottom;
let use_native = fully_visible && !ctx.is_scrolling && !ctx.modal_open;
if use_native { paint_native(...) } else { paint_scratch_partial(...) }
```

Any partial visibility therefore takes `paint_scratch_partial` (`:485`), which
cell-copies the pre-rendered **halfblocks** scratch — 1 pixel per column, 2 per
row. Kitty / Sixel / iTerm2 would hand the terminal the native payload;
halfblocks downsamples it to the text grid. That is the reported blur.

Two of the three triggers are permanent rather than transient:

| Trigger | Duration |
|---|---|
| partially scrolled (top above, or bottom below, the viewport) | while clipped |
| reserved height > viewport height (`images.max_height` defaults to 24, `src/config/sections.rs:221`, and is not clamped to the document area) | **forever** — `fully_visible` can never be true |
| within `SCROLL_QUIESCE` (150 ms, `src/app/frame_timer.rs:12`) | transient |

The third case is now partly mitigated, not fixed: `editor.max_width_enabled`
defaults to `true` with `max_width_cols = 100` (`src/config/sections.rs:123-124`),
and `clamp_doc_area_to_max_width` (`src/ui/editor_view.rs:100`) narrows the
document area, so the reserved height is computed against a ≤100-column box on a
wide terminal. That reduces how often an image exceeds the viewport, but a tall
portrait image on a short terminal still hits the permanent case.

The clipping is **vertical only** — `rect.x == area.x` and `rect.width ==
area.width` always (`build_snapshots` rect construction, `src/ui/image_view.rs:161`).
So the whole problem reduces to a row band, and the horizontal dimension never
needs any arithmetic at all.

## The interface: one band, every protocol

### The band

Every protocol has to answer the same two questions: *which rows of the image
should the terminal draw*, and *which cells should they land in*. The answers
are derived entirely from data the snapshot already carries — `natural_top`
(`isize`, deliberately negative once the top has scrolled out) and
`rect.height`, which is the reserved height `R`:

```
skip     = max(0, V0 - T)                        rows clipped off the top
visible  = min(T + R, V1) - max(T, V0)           rows that can be drawn
dst      = Rect(rect.x, max(T, V0), rect.width, visible)
```

`paint_scratch_partial` already computes a fragment of this (`clip_top`,
`src/ui/image_view.rs:506`) — it just spends it on a halfblocks cell copy
instead of on the protocol's row offset.

### The band is the only path, not a branch

A fully visible image yields `skip = 0, dst = rect` — **the band path degenerates
to exactly today's behaviour**. So the band should not be a second path beside
the `fully_visible` check; it should *be* the path, and `fully_visible` (`:348`)
should be deleted rather than joined.

`is_scrolling` does **not** retire, though — a correction to an earlier revision of
this plan, which read the gate as being about re-encoding. Its stated purpose is
the re-*composite*: during scroll every protocol falls back to
position-independent halfblocks because Kitty's placeholders "still re-composite
at each new cell position, the dominant source of scroll lag on image-heavy
documents" (the scroll gate's own comment in `src/ui/image_view.rs`). Row
addressing removes the re-encode but not the re-composite, so the scroll window
still applies to Kitty. What changes is what happens when scrolling *stops*: the
band paints at whatever offset the view came to rest at, instead of requiring the
image to have become fully visible again.

### Why the band must stay a render-time parameter

This is the property that makes the interface cheap, and it is worth stating
because the obvious alternative violates it.

`get_protocol_pair` caches protocol objects by `(url, width, height)`
(`src/image/cache.rs:255`). With the band as a *render-time* parameter, the
protocol's height stays the full reserved size, so scrolling changes none of the
key — the protocol is built once and reused, and a scroll tick costs one integer.
Band re-encoding (the M3 approach) makes the band part of the *encoding*, so the
key grows a dimension, the cache churns on every scroll settle, and the pair must
be rebuilt — which, for Kitty, is not merely expensive (limitation 1).

**Keep the band out of the cache key.** "Rebuild triggers" below records why the
geometry the key holds is in fact stable, and the one case where it is not.

## Backends

`ratatui_image::sliced` is already this interface — one `SlicedProtocol` variant
per protocol, plus `SignedPosition` (an `i16` position, so a negative top is
expressible) and `SlicedImage::skip_and_drop` (private, and already unit-tested
upstream against negative positions). All four backends are **free at render
time**; they differ in build cost, build timing, and payload accounting.

| Protocol | Upstream implementation | Build | Render | Accounting | Verdict |
|---|---|---|---|---|---|
| **Kitty** | `Kitty::render_with_skip` (row addressing via unicode placeholders) | 1 raw-RGBA transmit string (~1.3 MB for a full-width image) | no re-encode; per-row placeholder symbol | **none needed** — `Arc<AtomicBool>` transmit latch | **adopt (M1)** |
| **Sixel** | `SlicedSixel` (splices the payload's 6-px bands at draw time) | 1 sixel encode + band split | no re-encode; one string build | none | adopt (M2) |
| **iTerm2** | `Sliced(Vec<Protocol>)` (one PNG per text row) | **N PNG encodes** | no re-encode | **must be reworked** — N payload cells revive the `Buffer::diff` `invalidated` cascade | defer (M3) |
| **Halfblocks** | `Halfblocks::render_with_skip` (row copy) | 1 encode | no re-encode | none | **never** — zero fidelity gain; `paint_scratch_partial` already does exactly this |

The capability is therefore *not* what separates the protocols; the cost profile
is. Kitty and Sixel get the feature for free, iTerm2 pays for it in accounting,
and halfblocks has nothing to gain.

### Kitty — M1

Row addressing: the transmit payload contains every row, and the placeholder
grid addresses image rows by diacritic index (`row_y = y + skip_line_count`,
`ratatui-image-11.0.6/src/protocol/kitty.rs:186`). Drawing a band is a matter of
starting the grid at `skip` — no re-encode, no re-transmit, pixel-exact.

`render_with_skip(area, buf, skip)` takes no `drop`; the destination height
encodes it, which is consistent with the invocation below.

### Sixel — M2

The private `sixel_slice::SlicedSixel` (its module is *not* `pub`, so the type
cannot be named from this crate — but `SlicedProtocol::Sixel(…)` is a public
variant built by `SlicedProtocol::new_with_resize`, which is all M2 needs)
deconstructs the sixel payload into its native 6-pixel bands at build time and
skips/truncates them at draw time. Not pixel-accurate (6-px granularity), which
upstream documents as "good enough". The module comment also explains why the
generic `Sliced(Vec<…>)` path is *not* used for sixel: it glitches in foot.

### iTerm2 — M3, deferred

Structurally supported: pre-slice into one protocol per text row, render the
subset. Two costs make it unattractive now: the build is N PNG encodes, and each
row's escape lands in a cell symbol, which is precisely the situation edamame
already had to build `NativePaint` + `mark_rect_skipped` + the `Cell::PartialEq`
dependency to suppress (`docs/dev/media-export.md`). Worth doing only if iTerm2
users report the blur.

## Why the existing path cannot be extended

`StatefulKitty::render_with_skip` (`protocol/kitty.rs:66`) is **`pub(crate)`**,
and `StatefulProtocol` exposes no skip entry point at all. edamame holds a
`ThreadProtocol` (which wraps `StatefulProtocol`), so the row-addressing
primitive is unreachable from this crate — for every protocol, not just Kitty.
Adding a skip parameter to `paint_native` is therefore not an option; the public
`sliced` module is the vehicle, and `pub mod sliced` is **not** feature-gated
(`lib.rs:162`), so it is available under the current
`default-features = false, features = ["crossterm"]`.

## Upstream mechanics that constrain the design

1. **The Kitty transmit payload is raw RGBA, not PNG.** `transmit_virtual`
   (`protocol/kitty.rs:224`) does `img.to_rgba8()` and base64-chunks the raw
   bytes with `f=32,t=d`. For a full-width image at 80×24 cells × 8×16 px font
   that is 640×384 px = 983 KB raw → **~1.3 MB of escape string**, built
   **synchronously** inside `Kitty::new`.
2. **`SlicedProtocol::new*` allocates a fresh random id per build.**
   `picker.new_protocol_raw` uses `rand::random()` (`picker.rs:245-250`), and
   there is **no `d=I` delete sequence anywhere in `protocol/kitty.rs`**. A
   rebuild therefore leaks the previous image in the terminal's graphics store.
   Today's `StatefulKitty` avoids this by *reusing* its id across
   `resize_encode`. ⇒ **Build the sliced protocol once per geometry; a band
   change must never trigger a rebuild.**
3. **`Picker::is_tmux` has no public accessor** (`protocol_type()` and
   `font_size()` do), so a hand-rolled `Kitty::new` would have to re-derive tmux
   detection. Use the public `SlicedProtocol::new_with_resize`, which reads it
   internally, and accept the random id (limitation 1).
4. **`SlicedImage::render(area, buf)` takes the whole area plus a signed
   position**, and computes skip/drop itself with `area_top` hard-coded to 0.

## Design (M1 — the Kitty backend)

### Data flow

```
decode worker (src/app/image_dispatch.rs:551)     ← mirrors the existing scratch build
  SlicedProtocol::new_with_resize(picker, image, Size::new(width, rows), Resize::Fit(None))
  → LoadedImage.sliced = Some((rect, sliced))

get_protocol_pair (UI, cold path)
  claims prebuilt_sliced[(url, w, rows)]          ← same claim-once pattern as prebuilt_scratches
  (sync fallback only on a key miss — see "Rebuild triggers")

paint_images (UI, per frame)
  clear_visible_reserved_rect(snap, …)            ← unchanged; still needed for placeholder bleed
  SlicedImage::new(&sliced, SignedPosition { x: 0, y: -(skip as i16) })
      .render(dst, buf)
```

### The invocation, precisely

`SlicedImage::render(area, buf)` treats **`area` as the clipping window**, not as
"where the document is": it computes `skip' = max(0, -position.y)` and
`drop = max(0, position.y + size.height - area.height)`, then paints
`size.height - skip' - drop` rows at `area.y + max(0, position.y)`.

So with the protocol built at `S = (width, R)` and `area = dst` (height `V`):

```rust
// skip = rows of the image above the visible band; dst = the clamped visible rect
SlicedImage::new(sliced, SignedPosition { x: 0, y: -(skip as i16) }).render(dst, buf);
```

gives `skip' = skip`, `drop = R - skip - V`, and therefore exactly `V` rows
painted at `dst.y` — the band. Passing `ctx.area` instead of `dst` also works
for pure scrolling, but `dst` is the form that generalizes (next subsection).

### Rebuild triggers: the rect height is stable

An earlier revision of this plan flagged the `$$...$$` live preview as a rebuild
trigger, on the theory that `build_snapshots` shrinks the image rect mid-reveal
(`src/ui/image_view.rs:136`, `ImageReveal` at `src/editor/state.rs:121`). It does
not, and the reason is worth recording, because the implementation keys the
protocol as `(url, width, height)` like everything else *because* of it:

- A revealed `$$...$$` block's row override returns
  `reveal.rows + reveal.preview_rows` (`src/editor/state.rs:969`), and
  `build_snapshots` subtracts `source_rows_below = reveal.rows`, leaving
  `preview_rows`.
- `preview_rows` is `images.aspect_rows(url, …)` (`src/editor/state_cursor_block.rs:202`),
  documented as "same row count the renderer's override gives the image outside
  the reveal, so it doesn't resize when the reveal opens".
- A block that is *not* revealed takes `images.reserved_rows(url, …)`
  (`src/editor/state.rs:982`).
- For a decoded image those two are the same call: `reserved_rows` and
  `aspect_rows` both return `aspect_rows_of(…)`, differing only in what they
  answer for a `Failed` decode.

So the image rect has the same height before and during the reveal; the reveal
only adds source rows *below* it. No new key, no rebuild, no geometry
indirection — `paint_images` resolves the pair by `snap.rect.height`.

(The equality holds for a decoded image. For a `Failed` one they differ, since
`reserved_rows` collapses it to a single row — but a failed decode has no protocol
at all, so `get_protocol_pair` answers `None` before the key matters.)

**Accepting one synchronous build per image per new geometry.** A terminal resize
is the remaining trigger, and it cannot be avoided without new plumbing:

- `on_resize` (`src/app/event_loop.rs:607`) calls only
  `invalidate_native_paints()`. It does **not** clear `protocols` or
  `prebuilt_scratches`.
- `request` (`src/image/cache.rs:202`) is a no-op once a URL is decoded, so the
  decode worker never re-runs and **no fresh prebuilt is ever produced for a new
  geometry** — stale-keyed entries simply sit unmatched.
- Every `(image, geometry)` pair therefore pays exactly one synchronous build, in
  the first frame that paints it. That is the path already calibrated as "~5-20 ms
  sync encode here, rare enough not to regress scroll" (`src/image/cache.rs:276`).

For the halfblocks scratch, ~5-20 ms per image per resize is today's accepted
cost. For Kitty the same moment builds the transmit string; the estimate is the
same order (a `to_rgba8` of ~1 MB plus base64 of the same, plus `String` growth) —
**estimated, not measured**, and on the UI thread. Kitty's exposure is also wider
than the scratch path's, since `fully_visible` no longer gates it (though
`is_scrolling` still does, for the reason below).

Both synchronous paths are now instrumented (`tracing::debug!` under the existing
`[dev] logging` flag, carrying `micros`), so the number is measurable rather than
assumed. If it measures badly, the fix is to re-derive the prebuilt off-thread
from the already-cached `Arc<DynamicImage>` — a "rebuild prebuilt for
`(url, w, h)`" job on the **existing** decode worker, which would also remove
today's scratch hitch, and which stays inside this document's rejection of a
*second* channel.

### Threading: build on the decode worker

`SlicedProtocol::new*` builds the transmit string synchronously, so it must not
run on the UI thread — today that cost sits on the encoder worker. The decode
worker already does exactly this kind of one-time derived work and already holds
every input:

- `LoadedImage { url, image, scratch: Option<(Rect, Buffer)> }` (`src/image/loader.rs:25`)
- the scratch build at `src/app/image_dispatch.rs:551`, inside
  `dispatch_image_decodes_for` (`:408`), alongside `scratch_picker`,
  `scratch_width`, `max_cells`, `font_size`, and wrapped in
  `ExpectedPanic::new()` + `catch_unwind`

So: add `sliced: Option<(Rect, SlicedProtocol)>` to `LoadedImage`, populate it in
that same block **only when `scratch_picker.protocol_type() ==
ProtocolType::Kitty`**, and give `ImageCache` a `prebuilt_sliced` map mirroring
`prebuilt_scratches` (`src/image/cache.rs:146`, claimed at `:280`).

This deliberately does **not** touch the existing encoder channel or
`ThreadProtocol`. The channel type is upstream's concrete
`Sender<ratatui_image::thread::ResizeRequest>`, and `ThreadProtocol::new`
requires exactly that type; widening it to an edamame-owned enum would drag in
every `ThreadProtocol` user (`app.rs` field, `event_loop.rs` worker, `nav.rs`
attach, the `cache.rs` FIFO routing, and the `image_view.rs` test harness).

`get_protocol_pair` (`src/image/cache.rs:255`) needs **no new parameter**: the
protocol test is `native_picker.protocol_type() == ProtocolType::Kitty`, and the
picker has already had `resolve_protocol`'s Kitty→Iterm2 override applied to it
(`src/terminal/capabilities.rs:276`).

### Accounting

`NativePaint` (`src/image/cache.rs:100`) / `mark_rect_skipped`
(`src/ui/image_view.rs:473`) exist because iTerm2 re-emits the whole PNG on every
render. Kitty has an `Arc<AtomicBool>` transmit latch
(`KittyProtoState::make_transmit`), so the sliced path needs none of it: it
writes the placeholder cells each frame and ratatui's diff drops them because the
content is identical. iTerm2 / Sixel keep `paint_native`
(`src/ui/image_view.rs:398`) unchanged.

The symbol-width concern does not bite: a placeholder row's symbol is roughly
`area.width` display columns, so `Buffer::diff`'s `invalidated` stays bounded by
the area width — the same bound today's Kitty path already has.

### What is deliberately not done

- No `is_tmux` re-derivation, no hand-rolled `Kitty::new`, no id control.
- No `d=I` cleanup on eviction (needs a deferred-escape queue; limitation 1).
- No change to the halfblocks scratch, `paint_native`, or
  `paint_scratch_partial` for the other protocols.
- No new tuning knob or config surface.
- No geometry indirection for the pair lookup: unnecessary, per "Rebuild
  triggers".
- `image_band()` *was* extracted, contrary to this document's first guess. It is
  not duplication of `SlicedImage` — that widget derives `drop` from the area it
  is handed, while `image_band` produces the *inputs* (`skip` and the destination
  rect). Keeping them separate is what makes the clip arithmetic unit-testable at
  all, since `skip_and_drop` is private upstream.

## Changes by file

| File | Change |
|---|---|
| `src/image/loader.rs` | `LoadedImage` gains `sliced: Option<(Rect, SlicedProtocol)>` |
| `src/app/image_dispatch.rs` | populate `sliced` in the existing scratch-build block (`:551`), gated on the picker being Kitty, inside the same `catch_unwind` |
| `src/image/cache.rs` | `ImageCache` gains `prebuilt_sliced: HashMap<(String, u16, u16), SlicedProtocol>`; `ProtocolPair` gains `kitty_sliced: Option<SlicedProtocol>`; the Kitty cold path claims the prebuilt entry and **skips building the `ThreadProtocol`**, so there is no wasted encode and no duplicate 1.3 MB payload |
| `src/ui/image_view.rs` | new `image_band` (the clip arithmetic) and `paint_kitty_sliced`; `paint_images` routes Kitty through it before the `use_native` gate. `fully_visible` stops applying to Kitty; `is_scrolling` and `modal_open` still do |
| `src/image/mod.rs` | re-export `SlicedProtocol` / `SignedPosition` |
| `src/app/event_loop.rs` | pass `loaded.sliced` into `set_decoded_with_prebuilt` alongside `loaded.scratch` |
| `docs/dev/media-export.md` | add a bullet to that file's invariants list recording that Kitty bands at paint time and that the partial-visibility → scratch fallback no longer applies to it |

## Verification

1. **The clip arithmetic** (unit, `image_band_reports_the_visible_slice`): six
   cases — fully visible, top-clipped, bottom-clipped, both-clipped, off the top,
   off the bottom — plus two against a viewport that does not start at row zero,
   which is what catches an implementation measuring against the screen instead of
   the document area. `skip_and_drop` is private upstream, so this tests the inputs
   we derive, not upstream's `drop`.
2. **Paint routing** (integration, `kitty_paints_a_clipped_image_as_a_band`): a
   clipped snapshot writes a `\u{10EEEE}` placeholder — the row-addressed path ran,
   not the halfblocks scratch.
3. **No rebuild across a band change** (same test): the first frame carries the
   payload (`_Gq=2`) and the clipped frame does not, which is only possible if the
   same `SlicedProtocol` was reused. That pins the no-rebuild property and
   limitation 1 together, and it subsumes the reveal case — a reveal does not move
   the key at all ("Rebuild triggers").
4. **The two surviving gates**
   (`kitty_yields_the_band_while_scrolling_and_under_a_modal`): asserted with a
   *fully visible* image, so that only the gate under test can explain a scratch
   paint.
5. **The protocol gate and the prebuilt claim**
   (`build_kitty_sliced_only_answers_for_kitty`,
   `kitty_prebuilt_is_claimed_at_matching_dims_and_skips_the_threaded_protocol`) —
   the second of which also pins that Kitty builds no threaded protocol.
6. **Cold-path cost instrumentation** — landed as its own commit *before* the
   feature (`perf: time the synchronous halfblocks scratch fallback`), covering
   the scratch and, once it existed, the Kitty sliced build, under the existing
   `[dev] logging` flag. The numbers have not been read: no Kitty terminal was
   available here.
7. **Regression**: existing assertions
   (`two_native_images_transmit_once_then_go_quiet`,
   `a_scratch_frame_forces_the_next_native_frame_to_retransmit`, …) pass
   unchanged — the `]1337;File=`-based ones are iTerm2-only. `cargo test
   --no-fail-fast` gives 3188 passed / 0 failed / 12 ignored against a 3183/0/12
   baseline: exactly the five added tests, so nothing else moved. `cargo clippy
   --all-targets -- -D warnings` is clean, as is `cargo fmt`.
8. **Not done here**: the manual check on real Kitty/ghostty hardware. The paint
   path is pinned at the buffer level by items 1–5, but "does it actually look
   sharp on a terminal" is unverified, along with the resize-stall question that
   item 6's log exists to answer.

## Known limitations

1. **Random id per build, no delete.** Every rebuild leaves the previous image id
   resident in the terminal until Kitty evicts it. The build-geometry decision
   above leaves a terminal resize as the only trigger — removing the
   reveal-driven rect change is exactly what it buys — so the leak is bounded by
   resize count rather than by cursor movement. Fixing it properly needs a
   deferred `d=I` queue flushed on the next frame.
2. **`SlicedProtocol` must be `Send`** to cross the decode worker's channel.
   Expected (the `Kitty` payload is `Arc<AtomicBool>` + `String` + `Size`), but
   it is the first thing to confirm in code — if it fails, the fallback is to
   ship the `Kitty` alone and wrap it into `SlicedProtocol::Kitty` on the UI
   side (the enum's variants are public).
3. **Kitty-compatible terminals without unicode placeholders** would render
   nothing. Already handled upstream of this change: `resolve_protocol`
   (`src/terminal/capabilities.rs:276`) maps a probed `Kitty` to `Iterm2` when
   `iterm2_hint_is_trustworthy()` (`:263`), because iTerm2 answers the Kitty
   probe but cannot do placeholders.
4. The sliced path renders at most one viewport's worth of rows natively; an
   image taller than the terminal cannot show more than a screenful at once.
   That is inherent, not a regression.
5. Sixel band granularity is 6 px, so its `skip` is approximate (M2).
6. **One synchronous build per image per new geometry.** After a resize nothing
   re-derives the prebuilt map (`request` is a no-op once a URL is decoded), so
   each visible image pays one synchronous Kitty build on the UI thread. Bounded
   and once-per-resize, but estimated rather than measured — Verification item 4
   is the gate, and "Rebuild triggers" holds the alternative.

## Alternatives considered

- **Poke a skip parameter into `paint_native`.** Impossible: the primitive is
  `pub(crate)` and `StatefulProtocol` has no skip entry. Would require vendoring
  the protocol writers.
- **Band re-encode for every protocol (M3).** Uniform, and the only option for
  iTerm2, but it puts the band in the cache key and re-encodes on every scroll
  settle. Rejected as the *interface*; retained as the deferred iTerm2 backend.
- **A second encoder channel for the sliced build.** Rejected: the decode worker
  already produces the analogous `prebuilt_scratch` and already holds every
  input, so a second channel and worker are pure duplication.
- **Synchronous build in `get_protocol_pair`.** Rejected — it would put the
  1.3 MB string build on the UI thread. Kept only as the rare-fallback path, and
  even there it should be avoided (see "Rebuild triggers").
- **Clamp `images.max_height` to the document area.** Removes the permanent case
  cheaply, but changes layout semantics, forces a reparse, and still leaves the
  scrolled case blurry — and the band interface subsumes it.
- **Switch halfblocks to the sliced backend too.** No fidelity gain; the
  existing scratch path is the same row copy.

## Staging

| | Scope | Trigger to do it |
|---|---|---|
| **M1** | Kitty backend (this branch) | now |
| **M2** | Sixel backend (`SlicedSixel`); extract `image_band()` if the backend needs it outside `SlicedImage` | after M1 is verified on real hardware |
| **M3** | iTerm2 backend, incl. reworking the payload accounting | only if iTerm2 users report the blur |

Also worth landing independently of all three, as measurement rather than
mechanism, and distinct from Verification item 4's fallback-cost log: log the
`:348` decision (`protocol`, `fully_visible`, `is_scrolling`, band numbers, image
pixel height) under the existing `[dev] logging` flag, so the split between the
transient scroll window and the permanent cases is known rather than assumed.

## Resolved decisions

- Worktree location: `~/worktrees/edamame/<branch>` (outside the repo) — chosen
  over an in-repo `.worktrees/` so no `.gitignore` entry and no commit on `main`
  are needed.
- Build strategy: on the **decode worker**, mirroring `prebuilt_scratch` — not
  synchronous in `get_protocol_pair`, and not a new encoder channel. Keeps the
  encode off the UI thread, matching the existing invariant and precedent.
- Interface: the **band is a render-time parameter shared by all backends**, and
  the only paint path — `fully_visible` is deleted rather than branched around.
- The Kitty protocol is keyed by the snapshot's `(url, width, height)` **like
  everything else**. The geometry indirection this document originally called for
  turned out to be unnecessary: a reveal does not change the image rect's height
  ("Rebuild triggers" carries the proof), so a resize is the only miss.
- `is_scrolling` and `modal_open` still gate Kitty. The planned removal of the
  scroll gate was wrong: it exists for re-compositing, not for re-encoding.
- `get_protocol_pair` takes **no new parameter**; the Kitty test is
  `native_picker.protocol_type()`, which already reflects `resolve_protocol`'s
  override.
- The post-resize synchronous build is **accepted and measured** rather than
  engineered around up front; re-deriving the prebuilt off-thread (on the
  existing decode worker) is the documented follow-up if the number is bad.
