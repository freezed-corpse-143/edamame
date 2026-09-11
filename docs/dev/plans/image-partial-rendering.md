# Partial image rendering — the visible band as the interface

Branch: `image-kitty-row-addressing` (this branch implements **M1: the Kitty backend**)
Issue: [mijowi/edamame#50](https://github.com/mijowi/edamame/issues/50)

## Problem

`paint_images` gates the native graphics protocol on the image's **full reserved
rect** being inside the viewport (`src/ui/image_view.rs:370,411`):

```rust
let fully_visible = top >= viewport_top && bottom <= viewport_bottom;
let use_native = fully_visible && !ctx.is_scrolling && !ctx.modal_open;
if use_native { paint_native(...) } else { paint_scratch_partial(...) }
```

Any partial visibility therefore falls back to `paint_scratch_partial`
(`:579`), which cell-copies the halfblocks scratch — **1 pixel per column, 2 per
row**. Kitty / Sixel / iTerm2 hand the terminal the native payload; halfblocks
downsamples it to the text grid. That is the reported blur.

Two of the three triggers are permanent rather than transient:

| Trigger | Duration |
|---|---|
| partially scrolled (top above, or bottom below, the viewport) | while clipped |
| reserved height > viewport height (`images.max_height` defaults to 24, `src/config/sections.rs:334`, and is not clamped to the document area) | **forever** — `fully_visible` can never be true |
| within `SCROLL_QUIESCE` (150 ms, `src/app/frame_timer.rs:20`) | transient |

The clipping is **vertical only** — `rect.x == area.x` and `rect.width ==
area.width` always (`build_snapshots`, `src/ui/image_view.rs:150-160`). So the
whole problem reduces to a row band, and the horizontal dimension never needs
any arithmetic at all.

## The interface: one band, every protocol

### The band

Every protocol has to answer the same two questions: *which rows of the image
should the terminal draw*, and *which cells should they land in*. The answers
are five numbers derived entirely from data the snapshot already carries —
`rect.width` / `rect.height` (the reserved size `R`) and `natural_top` (`T`,
`isize`, deliberately negative when the top has scrolled out):

```
skip     = max(0, V0 - T)                        rows clipped off the top
visible  = min(T + R, V1) - max(T, V0)           rows that can be drawn
drop     = R - skip - visible                    rows clipped off the bottom
dst      = Rect(x, max(T, V0), width, visible)   screen rect for the band
```

`paint_scratch_partial` already computes a fragment of this (`clip_top`,
`src/ui/image_view.rs:601-611`) — it just spends it on a halfblocks cell copy
instead of on the protocol's row offset.

### The band is the only path, not a branch

A fully visible image yields `skip = 0, drop = 0, dst = rect` — **the band path
degenerates to exactly today's behaviour**. So the band should not be a second
path beside the `fully_visible` check; it should *be* the path, and
`fully_visible` (`:411`) should be deleted rather than joined.

The same argument retires `is_scrolling` for the addressing backends: that gate
exists solely to avoid a per-frame re-encode, and row addressing does not
re-encode. It keeps its meaning for iTerm2, which does.

### Why the band must stay a render-time parameter

This is the property that makes the interface cheap, and it is worth stating
because the obvious alternative violates it.

`get_protocol_pair` caches protocol objects by `(url, width, height)`. With the
band as a *render-time* parameter, `height` remains the **full reserved size**
`R`, so scrolling changes none of the key — the protocol is built once and
reused for the life of the pair, and a scroll tick costs one integer. Band
re-encoding (the M3 approach) makes the band part of the *encoding*, so the key
grows a dimension, the cache churns on every scroll settle, and the pair must be
rebuilt — which, for Kitty, is not merely expensive (see limitation 1).

**Keep the band out of the cache key.**

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
is. Kitty and Sixel get the feature for free, iTerm2 pays for it in
accounting, and halfblocks has nothing to gain.

### Kitty — M1

Row addressing: the transmit payload contains every row, and the placeholder
grid addresses image rows by diacritic index (`row_y = y + skip_line_count`,
`ratatui-image-11.0.6/src/protocol/kitty.rs:186`). Drawing a band is a matter of
starting the grid at `skip`. No re-encode, no re-transmit, pixel-exact.

`render_with_skip(area, buf, skip)` takes no `drop` — `area.height` already
encodes it, which is consistent with `dst` above.

Bonus, and not incidental: this removes **both** permanent triggers for Kitty
images, including the "reserved height exceeds the viewport" case where
`fully_visible` could never be satisfied.

### Sixel — M2

The private `sixel_slice::SlicedSixel` (its module is *not* `pub`, so the type
cannot be named from this crate — but `SlicedProtocol::Sixel(…)` is a public
variant built by `SlicedProtocol::new_with_resize`, which is all M2 needs)
deconstructs the sixel payload into its native 6-pixel bands at build time and
skips/truncates them at draw time. Not
pixel-accurate (6-px granularity), which upstream documents as "good enough".
The module comment also explains why the generic `Sliced(Vec<…>)` path is *not*
used for sixel: it glitches in foot.

### iTerm2 — M3, deferred

Structurally supported: pre-slice into one protocol per text row, render the
subset. Two costs make it unattractive now: the build is N PNG encodes, and each
row's escape lands in a cell symbol, which is precisely the situation edamame
already had to build `NativePaint` + `mark_rect_skipped` + the `Cell::PartialEq`
dependency to suppress. Worth doing only if iTerm2 users report the blur.

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
   `resize_encode`. ⇒ **Build the sliced protocol once per `(url, width,
   height)`; a band change must never trigger a rebuild.**
3. **`Picker::is_tmux` has no public accessor** (`protocol_type()` and
   `font_size()` do), so a hand-rolled `Kitty::new` would have to re-derive tmux
   detection. Use the public `SlicedProtocol::new_with_resize`, which reads it
   internally, and accept the random id (limitation 1).
4. **`SlicedImage::render(area, buf)` takes the whole area plus a signed
   position** and computes skip/drop itself. Pass the *document area*, not the
   clamped band: `skip_and_drop` treats `area_top` as 0, so `image_area` comes
   out clamped correctly on its own.

## Design (M1 — the Kitty backend)

### Data flow

```
decode worker (src/app/image_dispatch.rs)     ← mirrors the existing scratch build
  SlicedProtocol::new_with_resize(picker, image, Size::new(width, rows), Resize::Fit(None))
  → LoadedImage.sliced = Some((rect, sliced))

get_protocol_pair (UI, cold path)
  claims prebuilt_sliced[(url, w, h)]          ← same claim-once pattern as prebuilt_scratches
  (sync fallback only on a key miss, e.g. the terminal was resized since dispatch)

paint_images (UI, per frame)
  clear_visible_reserved_rect(snap)            ← unchanged; still needed for placeholder bleed
  SlicedImage::new(&sliced, SignedPosition { x: 0, y: natural_top - area.y })
      .render(area, buf)                       ← skip/drop computed upstream
```

Sketch of the dispatch that replaces the `use_native` branch:

```rust
match protocol {
    ImageProtocol::KittyGraphics if kitty_sliced_ready => {
        SlicedImage::new(kitty_sliced, position).render(ctx.area, ctx.buf);
    }
    _ => { /* today's fully_visible / is_scrolling branch, unchanged */ }
}
```

### Threading: build on the decode worker

`SlicedProtocol::new*` builds the ~1.3 MB string synchronously, so it must not
run on the UI thread — today that cost sits on the encoder worker. The decode
worker already does exactly this kind of one-time derived work and already holds
every input:

- `LoadedImage { url, image, scratch: Option<(Rect, Buffer)> }` (`src/image/loader.rs:34`)
- the scratch build at `src/app/image_dispatch.rs:640-660`, alongside
  `scratch_picker`, `scratch_width`, `max_cells`, `font_size`, wrapped in
  `ExpectedPanic::new()` + `catch_unwind`

So: add `sliced: Option<(Rect, SlicedProtocol)>` to `LoadedImage`, populate it in
that same block **only when `scratch_picker.protocol_type() ==
ProtocolType::Kitty`**, and give `ImageCache` a `prebuilt_sliced` map mirroring
`prebuilt_scratches` (`src/image/cache.rs:412-423`).

This deliberately does **not** touch the existing encoder channel or
`ThreadProtocol`. The channel type is upstream's concrete
`Sender<ratatui_image::thread::ResizeRequest>`, and `ThreadProtocol::new`
requires exactly that type; widening it to an edamame-owned enum would drag in
every `ThreadProtocol` user (`app.rs` field, `event_loop.rs` worker, `nav.rs`
attach, the `cache.rs` FIFO routing, and the `image_view.rs` test harness). A
second channel and worker are unnecessary when the decode worker is already the
right home and already has the precedent.

### Accounting

`NativePaint` / `mark_rect_skipped` exist because iTerm2 re-emits the whole PNG
on every render. Kitty has an `Arc<AtomicBool>` transmit latch
(`KittyProtoState::make_transmit`), so the sliced path needs none of it: it
writes the placeholder cells each frame and ratatui's diff drops them because
the content is identical. iTerm2 / Sixel keep `paint_native` unchanged.

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
  for something other than `SlicedImage` (M2/iTerm2), and note that its absence
  means the arithmetic is covered by integration assertions rather than a unit
  test — `skip_and_drop` is private upstream.

## Changes by file

| File | Change |
|---|---|
| `src/image/loader.rs` | `LoadedImage` gains `sliced: Option<(Rect, SlicedProtocol)>` |
| `src/app/image_dispatch.rs` | populate `sliced` in the existing scratch-build block, gated on the picker being Kitty, inside the same `catch_unwind` |
| `src/image/cache.rs` | `ImageCache` gains `prebuilt_sliced: HashMap<(String, u16, u16), SlicedProtocol>`; `ProtocolPair` gains `kitty_sliced: Option<SlicedProtocol>`; `get_protocol_pair` claims the prebuilt entry (sync fallback on miss) and **skips building the `ThreadProtocol`** for Kitty, so no wasted encode and no duplicate 1.3 MB payload |
| `src/ui/image_view.rs` | `paint_images` routes Kitty to `SlicedImage` before the `use_native` gate; `fully_visible` / `is_scrolling` stop applying to Kitty |
| `src/image/mod.rs` | re-export `SlicedProtocol` / `SignedPosition` |
| `docs/dev/media-export.md` | update the decision table: `Partially visible → scratch` no longer holds for Kitty |

## Verification

1. **Position arithmetic** (unit, `src/ui/image_view.rs`): for the five cases —
   fully visible, top-clipped, bottom-clipped, both-clipped, entirely
   off-screen — assert the `(skip, drop)` implied by `natural_top - area.y`.
   `skip_and_drop` is private upstream, so this tests our own positions.
2. **Paint routing** (integration, existing `Harness` in
   `src/ui/image_view.rs`): add a Kitty picker beside the existing
   `iterm2_picker()` / `halfblocks_picker()` (`Picker::from_fontsize` +
   `set_protocol_type(ProtocolType::Kitty)`). Assert that a *partially visible*
   snapshot writes a `\u{10EEEE}` placeholder into the visible band's first cell
   (rather than halfblock cells), that the clamped rect is the visible band, and
   that rows outside the band are untouched.
3. **iTerm2 / halfblocks regression**: existing assertions
   (`two_native_images_transmit_once_then_go_quiet`,
   `a_scratch_frame_forces_the_next_native_frame_to_retransmit`, …) must keep
   passing. The Kitty semantics change deliberately — the `]1337;File=`-based
   assertions are iTerm2-only and unaffected.
4. **No rebuild on scroll**: assert the protocol pair is the *same object* (or
   that no `rand`-fresh id was minted) across a scroll that only moves
   `natural_top`. This is the regression guard for limitation 1.
5. **Manual**: real Kitty/ghostty, scroll a tall image; confirm no blur at rest
   and none mid-scroll, and that the 150 ms window no longer downgrades.
6. **Full suite**: `cargo test --no-fail-fast`, plus
   `cargo clippy --all-targets -- -D warnings`.

## Known limitations

1. **Random id per build, no delete.** A rebuild (terminal resize, changed
   reserved height) leaves the previous image id resident in the terminal until
   Kitty evicts it. Bounded by the number of resizes, and a consequence of
   upstream's API shape; fixing it needs a deferred `d=I` queue flushed on the
   next frame.
2. **`SlicedProtocol` must be `Send`** to cross the decode worker's channel.
   Expected (the `Kitty` payload is `Arc<AtomicBool>` + `String` + `Size`), but
   it is the first thing to confirm in code — if it fails, the fallback is to
   ship the `Kitty` alone and wrap it into `SlicedProtocol::Kitty` on the UI
   side (the enum's variants are public).
3. **Kitty-compatible terminals without unicode placeholders** would render
   nothing. Already handled upstream of this change: `resolve_protocol` maps a
   probed `Kitty` to `Iterm2` when `iterm2_hint_is_trustworthy()`
   (`src/terminal/capabilities.rs:355`), because iTerm2 answers the Kitty probe
   but cannot do placeholders.
4. The sliced path renders at most one viewport's worth of rows natively; an
   image taller than the terminal cannot show more than a screenful at once.
   That is inherent, not a regression.
5. Sixel band granularity is 6 px, so its `skip` is approximate (M2).

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
mechanism: log the `:411` decision (`protocol`, `fully_visible`, `is_scrolling`,
band numbers, image pixel height) under the existing `[dev] logging` flag, so the
split between the transient scroll window and the permanent cases is known
rather than assumed.

## Resolved decisions

- Worktree location: `~/worktrees/edamame/<branch>` (outside the repo) — chosen
  over an in-repo `.worktrees/` so no `.gitignore` entry and no commit on `main`
  are needed.
- Build strategy: on the **decode worker**, mirroring `prebuilt_scratch` — not
  synchronous in `get_protocol_pair`, and not a new encoder channel. Keeps the
  encode off the UI thread, matching the existing invariant and precedent.
- Interface: the **band is a render-time parameter shared by all backends**, and
  the only paint path — `fully_visible` is deleted rather than branched around.
