# Kitty row addressing (M1) — sharp images while partially visible

Branch: `image-kitty-row-addressing`
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
area.width` always (`build_snapshots`, `src/ui/image_view.rs:150-160`) — so the
whole problem reduces to a row band: `skip` rows scrolled off the top, `drop`
rows off the bottom.

## Scope

**In:** Kitty (and Kitty-compatible: ghostty, wezterm) renders a row-sliced view
natively at any scroll position. Fixes both the transient and the permanent
cases for those terminals, and removes the need for the scroll-time halfblocks
window there.

**Out:** Sixel (M2 — `SlicedSixel`), iTerm2 (M3 — band re-encode), and the
halfblocks fallback's own fidelity. iTerm2 / sixel keep today's behaviour
exactly.

## Why Kitty only, and why the existing path cannot do it

`StatefulKitty::render_with_skip` (`ratatui-image-11.0.6/src/protocol/kitty.rs:66`)
is **`pub(crate)`**, and `StatefulProtocol` exposes no skip entry point. edamame
holds `ThreadProtocol` (which wraps `StatefulProtocol`), so the row-addressing
primitive is unreachable from this crate. The public alternative is
`sliced::SlicedProtocol` + `SlicedImage`, both `pub`, and `pub mod sliced` is
**not** feature-gated (`lib.rs:162`) — available under the current
`default-features = false, features = ["crossterm"]`.

Mechanism: Kitty's unicode-placeholder rendering addresses image rows by
diacritic index (`row_y = y + skip_line_count`), so a *transmit once, draw any
row range* is available with no re-encode and no re-transmit.

## Upstream mechanics that constrain the design

1. **The transmit payload is raw RGBA, not PNG.** `transmit_virtual`
   (`protocol/kitty.rs:224`) does `img.to_rgba8()` and base64-chunks the raw
   bytes with `f=32,t=d`. For a full-width image at 80×24 cells × 8×16 px font
   that is 640×384 px = 983 KB raw → **~1.3 MB of escape string**, built
   **synchronously** inside `Kitty::new`.
2. **`SlicedProtocol::new*` allocates a fresh random id per build.**
   `picker.new_protocol_raw` uses `rand::random()` (`picker.rs:245-250`), and
   there is **no `d=I` delete sequence anywhere in `protocol/kitty.rs`**. So a
   rebuild leaks the old image in the terminal's graphics store. Today's
   `StatefulKitty` avoids this by *reusing* its id across `resize_encode`.
   ⇒ **Build the sliced protocol once per `(url, width, height)` and never
   rebuild it while the pair lives.**
3. **`Picker::is_tmux` has no public accessor** (`protocol_type()` and
   `font_size()` do), so a hand-rolled `Kitty::new` would have to re-derive
   tmux detection. Use the public `SlicedProtocol::new_with_resize`, which reads
   it internally, and accept the random id (see limitation 1 below).
4. **`SlicedImage::render(area, buf)` takes the whole area plus a signed
   position**, computing skip/drop itself via the (private) `skip_and_drop`.
   `SignedPosition.y: i16` supports a negative top, i.e. exactly "scrolled above
   the viewport" — already unit-tested upstream for those cases.

## Design

### Data flow

```
decode worker (image_dispatch.rs)          ← mirrors the existing scratch build
  SlicedProtocol::new_with_resize(picker, image, Size::new(width, rows), Resize::Fit(None))
  → LoadedImage.sliced = Some((rect, sliced))

get_protocol_pair (UI, cold path)
  claims prebuilt_sliced[(url, w, h)]     ← same claim-once pattern as prebuilt_scratches
  (sync fallback only when the key missed, e.g. terminal resized since dispatch)

paint_images (UI, per frame)
  clear_visible_reserved_rect(snap)       ← unchanged, still needed for placeholder bleed
  SlicedImage::new(&sliced, SignedPosition { x: 0, y: natural_top - area.y })
      .render(area, buf)
```

`area` is the **whole document area** (not the clamped band); `position.y` is
`natural_top - area.y` saturating to `i16`. `skip_and_drop` then yields
`skip = max(0, -y)` and `drop = max(0, y + height - area.height)`, and the
widget places the image at the clamped visible rect with the right row offset.

### The degenerate case is the current behaviour

Fully visible ⇒ `skip = drop = 0` ⇒ the widget renders the full rect. There is
no second code path to keep in sync.

### Threading: build on the decode worker

`SlicedProtocol::new*` builds the ~1.3 MB string synchronously, so it must not
run on the UI thread — today that cost is on the encoder worker. The decode
worker already does exactly this kind of one-time derived work and already
carries the inputs:

- `LoadedImage { url, image, scratch: Option<(Rect, Buffer)> }` (`src/image/loader.rs:34`)
- the scratch build at `src/app/image_dispatch.rs:640-660`, alongside
  `scratch_picker`, `scratch_width`, `max_cells`, `font_size`, wrapped in
  `ExpectedPanic::new()` + `catch_unwind`

So: add `sliced: Option<(Rect, SlicedProtocol)>` to `LoadedImage`, populate it in
the same block **only when `scratch_picker.protocol_type() ==
ProtocolType::Kitty`**, and give `ImageCache` a `prebuilt_sliced` map mirroring
`prebuilt_scratches` (`src/image/cache.rs:412-423`).

This deliberately does **not** touch the existing encoder channel or
`ThreadProtocol`: the channel type is upstream's concrete
`Sender<ratatui_image::thread::ResizeRequest>`, and `ThreadProtocol::new`
requires that exact type. Widening it to an edamame-owned enum would drag in
every `ThreadProtocol` user. A second channel/worker is unnecessary when the
decode worker is already the right home.

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
- No `d=I` cleanup on eviction (needs a deferred-escape queue; see limitation 1).
- No change to the halfblocks scratch, `paint_native`, or `paint_scratch_partial`
  for the other protocols.
- No new tuning knob or config surface.

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
   off-screen — assert the `(skip, drop)` implied by
   `natural_top - area.y`. `skip_and_drop` itself is private upstream, so this
   tests our own positions, not upstream's function.
2. **Paint routing** (integration, existing `Harness` in
   `src/ui/image_view.rs`): add a Kitty picker beside the existing
   `iterm2_picker()` / `halfblocks_picker()` (`Picker::from_fontsize` +
   `set_protocol_type(ProtocolType::Kitty)`). Assert that a *partially visible*
   snapshot writes a `\u{10EEEE}` placeholder into the visible band's first cell
   (rather than halfblock cells), that the clamped rect is the visible band, and
   that rows outside the visible band are untouched.
3. **iTerm2 / halfblocks regression**: existing assertions
   (`two_native_images_transmit_once_then_go_quiet`,
   `a_scratch_frame_forces_the_next_native_frame_to_retransmit`, …) must keep
   passing. Note the Kitty semantics change deliberately — the
   `]1337;File=`-based assertions are iTerm2-only and unaffected.
4. **Manual**: real Kitty/ghostty, scroll a tall image; confirm no blur at rest
   and no blur mid-scroll, and that the 150 ms window no longer downgrades.
5. **Full suite**: `cargo test --no-fail-fast`, plus
   `cargo clippy --all-targets -- -D warnings`.

## Known limitations

1. **Random id per build, no delete.** A rebuild (terminal resize, changed
   reserved height) leaves the previous image id resident in the terminal until
   Kitty evicts it. Bounded by the number of resizes, and strictly a
   consequence of upstream's API shape; fixing it needs a deferred `d=I` queue
   flushed on the next frame.
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

## Alternatives considered

- **Poke a skip parameter into the existing `paint_native`.** Impossible:
  `StatefulKitty::render_with_skip` is `pub(crate)`; `StatefulProtocol` has no
  skip entry. Would require vendoring the Kitty protocol writer.
- **Band re-encode for every protocol (M3).** Uniform, and the only option for
  iTerm2, but re-encodes on every scroll settle. Deferred; see the issue.
- **A second encoder channel for the sliced build.** Rejected: the decode worker
  already produces the analogous `prebuilt_scratch` and already holds every
  input, so a second channel/worker is pure duplication.
- **Clamp `images.max_height` to the document area (option E).** Removes the
  permanent case cheaply, but changes layout semantics and forces a reparse;
  orthogonal to M1 and still leaves the scrolled case blurry.

## Resolved decisions

- Worktree location: `~/worktrees/edamame/<branch>` (outside the repo) — chosen
  over an in-repo `.worktrees/` so no `.gitignore` entry and no commit on `main`
  are needed.
- Build strategy: **B2, on the decode worker** (not synchronous in
  `get_protocol_pair`) — keeps the encode off the UI thread, matching the
  existing invariant and the `prebuilt_scratch` precedent.
