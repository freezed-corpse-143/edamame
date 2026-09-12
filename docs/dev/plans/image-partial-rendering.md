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

The same argument retires `is_scrolling` for the addressing backends: that gate
exists solely to avoid a per-frame re-encode, and row addressing does not
re-encode. It keeps its meaning for iTerm2, which does.

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

**Keep the band out of the cache key.** See "Rebuild triggers" below for the one
place the current code violates this already.

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

### Rebuild triggers: the rect height is not stable

This is the one place the current architecture already puts geometry into the
cache key, and the math/figures feature added since this branch's original base
makes it reachable by ordinary editing.

`build_snapshots` now shrinks an image block's rect by `source_rows_below` when
that block is mid raw-reveal with the live preview on (`src/ui/image_view.rs:136`,
`ImageReveal` at `src/editor/state.rs:121`, `preview_rows` at `:129`). So
`rect.height` — and therefore the `(url, width, height)` protocol key — **changes
when the cursor moves into a figure**, not just on a terminal resize.

Consequences for M1:

- The prebuilt sliced protocol is produced at **decode time**
  (`image_dispatch.rs:551`), so a reveal (or resize) makes `get_protocol_pair`
  miss and fall into the **synchronous** cold path. For Kitty that path would
  build the ~1.3 MB transmit string **on the UI thread** — a hitch triggered by
  cursor movement. Today's fallback is cheap by comparison: it only allocates a
  `StatefulProtocol`, and the encode stays on the worker.
- Each rebuild mints a fresh random id and leaks the previous one, where today's
  `StatefulKitty` reuses its id (limitation 1).

**Decision:** build the Kitty sliced protocol at the **decode-time reserved
geometry** — the same `(width, rows)` the halfblocks scratch already uses, and
`reserved_rows` (`src/image/cache.rs:401`) already computes it — and express
*both* scrolling and the reveal as bands at paint time. Since `dst` is the
clipping window, the reveal needs no separate mechanism: it is `skip = 0` with a
shorter `dst`.

That implies one structural change beyond adding a field: `paint_images` must
resolve the Kitty pair by the protocol geometry rather than by
`snap.rect.height`, so a reveal reuses the existing pair instead of minting a new
one.

**Resolved: accept one synchronous build per image per geometry change, and
measure it.** The fallback cannot be removed without new plumbing, and the
mechanics turn out to be exact rather than open:

- `on_resize` (`src/app/event_loop.rs:607`) calls only
  `invalidate_native_paints()`. It does **not** clear `protocols` or
  `prebuilt_scratches`.
- `request` (`src/image/cache.rs:202`) is a no-op once a URL is decoded, so the
  decode worker never re-runs and **no fresh prebuilt is ever produced for a new
  geometry** — stale-keyed scratches just sit unmatched (`:145`).
- So every `(image, geometry)` pair pays exactly one synchronous build, in the
  first frame that paints it. That is the path already calibrated as "~5-20 ms
  sync encode here, rare enough not to regress scroll" (`:275-278`).

For the halfblocks scratch, ~5-20 ms per image per resize is the accepted cost
today. For Kitty the same moment would build the ~1.3 MB string; the estimate is
the same order (a `to_rgba8` of ~1 MB plus base64 of the same, plus `String`
growth), so the existing trade does not look broken — but it is **estimated, not
measured**, and it is on the UI thread. Kitty also has *more* exposure than the
scratch path, because M1 deletes the `fully_visible` gate: every visible image at
a new geometry triggers the build, not only the fully visible ones.

Land M1 with the synchronous fallback and instrument it (Verification item 4). If
it measures badly, the fix is to re-derive the prebuilt off-thread from the
already-cached `Arc<DynamicImage>` — a "rebuild prebuilt for `(url, w, h)`" job on
the **existing** decode worker, which would also remove today's 5-20 ms scratch
hitch. That stays inside this document's rejection of a *second* channel, since
it rides a worker that already exists and already returns `ImageReady`.

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
- No `image_band()` helper yet: `SlicedImage` computes skip/drop internally, so
  writing our own would be duplication. Extract it when a backend needs the band
  for something other than `SlicedImage` (M2 / iTerm2) — noting that its absence
  means the arithmetic is covered by integration assertions rather than a unit
  test, since `skip_and_drop` is private upstream.

## Changes by file

| File | Change |
|---|---|
| `src/image/loader.rs` | `LoadedImage` gains `sliced: Option<(Rect, SlicedProtocol)>` |
| `src/app/image_dispatch.rs` | populate `sliced` in the existing scratch-build block (`:551`), gated on the picker being Kitty, inside the same `catch_unwind` |
| `src/image/cache.rs` | `ImageCache` gains `prebuilt_sliced: HashMap<(String, u16, u16), SlicedProtocol>`; `ProtocolPair` gains `kitty_sliced: Option<SlicedProtocol>`; the Kitty cold path claims the prebuilt entry and **skips building the `ThreadProtocol`**, so there is no wasted encode and no duplicate 1.3 MB payload |
| `src/ui/image_view.rs` | `paint_images` routes Kitty to `SlicedImage` before the `use_native` gate, resolving the pair by the protocol geometry; `fully_visible` / `is_scrolling` stop applying to Kitty |
| `src/image/mod.rs` | re-export `SlicedProtocol` / `SignedPosition` |
| `docs/dev/media-export.md` | add a bullet to that file's invariants list recording that Kitty bands at paint time and that the partial-visibility → scratch fallback no longer applies to it |

## Verification

1. **The invocation's arithmetic** (unit, `src/ui/image_view.rs`): for the cases
   fully visible / top-clipped / bottom-clipped / both-clipped / entirely
   off-screen / rect-shrunk-by-reveal, assert the `(skip, dst)` pair and the
   `(skip', drop)` it implies. `skip_and_drop` is private upstream, so this tests
   our own derivation.
2. **Paint routing** (integration, the existing `Harness` at
   `src/ui/image_view.rs:589`, with the pickers at `:566`/`:573`): add a Kitty
   picker (`Picker::from_fontsize` + `set_protocol_type(ProtocolType::Kitty)`).
   Assert that a *partially visible* snapshot writes a `\u{10EEEE}` placeholder
   into the band's first cell (rather than halfblock cells), that the painted
   rect is the band, and that rows outside it are untouched.
3. **No rebuild on scroll or reveal**: assert the Kitty protocol pair is reused
   (and no fresh id minted) across a scroll that only moves `natural_top`, and
   across a reveal that only shrinks `rect.height`. This is the regression guard
   for the decision above and for limitation 1.
4. **Cold-path fallback cost** — instrumented, and the number the decision above
   rests on: log the synchronous fallback in `get_protocol_pair` (url, geometry,
   elapsed) under the existing `[dev] logging` flag, and record the real
   per-image cost of the Kitty sliced build against the halfblocks scratch. This
   is worth landing as its own small commit *before* M1, since the same log shows
   how often the fallback fires at all.
5. **iTerm2 / halfblocks regression**: existing assertions
   (`two_native_images_transmit_once_then_go_quiet`,
   `a_scratch_frame_forces_the_next_native_frame_to_retransmit`, …) must keep
   passing. The Kitty semantics change deliberately — the `]1337;File=`-based
   assertions are iTerm2-only and unaffected.
6. **Manual**: real Kitty/ghostty, scroll a tall image; confirm no blur at rest
   and none mid-scroll, and that the 150 ms window no longer downgrades. Also
   confirm that entering a figure's reveal does not hitch, and that a resize with
   several images on screen does not produce a visible stall.
7. **Full suite**: `cargo test --no-fail-fast`, plus
   `cargo clippy --all-targets -- -D warnings`.

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
- The Kitty protocol is keyed by the **decode-time reserved geometry**, not by
  the snapshot's (reveal-dependent) rect height, so neither scrolling nor a
  reveal rebuilds it.
- `get_protocol_pair` takes **no new parameter**; the Kitty test is
  `native_picker.protocol_type()`, which already reflects `resolve_protocol`'s
  override.
- The post-resize synchronous build is **accepted and measured** rather than
  engineered around up front; re-deriving the prebuilt off-thread (on the
  existing decode worker) is the documented follow-up if the number is bad.
