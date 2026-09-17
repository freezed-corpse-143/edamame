//! Spike: inline `$…$` LaTeX drawn as a terminal image on the text's own row.
//!
//! **This is a spike, not a feature.**  It exists to answer one question on a real terminal —
//! whether a one-cell-row-tall formula, placed with the kitty protocol's direct placement
//! (`a=p`) and padded internally so its baseline lands on the text's, reads as inline math — and
//! to find out what the layout side costs.
//!
//! How it works, and why that is enough:
//!
//! * The renderer asks for the formula's **measured cell width** ([`measure`], the ink at the
//!   em the raster will use) and emits exactly that many cells.  Reserving the *source's* width
//!   was the first cut: `$Y_1$` is five source characters and about three cells of ink, so every
//!   short formula left visible whitespace after itself.  Width == ink is what removes the gap,
//!   and it is the one change the wrap model has to absorb: the atom is still a run of
//!   single-width, unbreakable characters, just the right number of them.
//! * The atom is located by a sentinel ([`atom_span`]) rather than by re-finding `$source$` in
//!   the rendered text, so the painter needs no knowledge of the source's width.
//! * [`paint`] walks the visible rendered lines, finds each sentinel, and paints the rasterized
//!   formula over those cells: one erase sweep (the glyphs a buffer `Skip` cannot remove), one
//!   transmit the first time, and one short placement escape per frame in which the atom moved.
//! * A formula holding the cursor is laid out as its **source text** again — the reveal.  Since
//!   the source is wider than the ink, that re-wraps the line by the difference, exactly as the
//!   block-level reveal re-flows its rows.  `EditorState` answers *which* formula that is (see
//!   [`set_build_inputs`]); the renderer only asks.
//! * The bitmap is flattened onto the theme's document background rather than left transparent:
//!   the atom's cells hold text styled `code_span`, and the placement's erase sweep refills those
//!   cells with the background *colour* before the image lands — so a transparent formula would
//!   show that chip through its own empty pixels.  Display math flattens for the same reason and
//!   shares the helper.
//!
//! Gate: `EDAMAME_INLINE_MATH` overrides when set (`0`/`off` disables); unset, the terminal
//! decides — a direct-placement terminal (WezTerm or Windows Terminal) turns it on by itself,
//! everything else stays inert.  On top of that gate, the session's *Figures* consent decides
//! whether pictures may be drawn at all (`App::effective_diagrams_enabled`): an atom rasterizes on
//! the render path, so unlike a block figure it never reaches the decode dispatch where that
//! setting is otherwise enforced, and the App mirrors it into the build inputs for that reason.
//! Baseline sweep: `EDAMAME_INLINE_MATH_RATIO=0.7…0.9`.  All read once per process.
//!
//! Deliberate spike shortcuts, each with its upgrade path:
//! * the placement is *cell-aligned*; the sub-cell `Y` offset that would make the baseline exact
//!   is wired into [`placement_symbol`] but left at 0, because the padded raster answers the
//!   question first.  The probe proved `Y` is honoured 1:1 in device pixels, so the fallback is
//!   de-risked by measuring it on a real terminal, where it is honoured 1:1;
//! * the atom table, the build inputs and the bitmap cache are process-global (`thread_local`)
//!   rather than threaded through `ParsedDoc::build` and the decode worker: fine for a handful of
//!   formulas, wrong for a document full of them;
//! * atoms are measured on the render path (a RaTeX parse + layout per distinct formula, cached),
//!   so a keystroke in a document with many formulas pays for each of them once;
//! * the atom's width is measured at one cell size, which the session probes once — a font change
//!   mid-session would need the layout invalidated.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::ops::Range;

use ratatui::buffer::{Buffer as TuiBuf, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::editor::EditorState;

/// Kitty's per-command base64 payload limit; chunks mirror the upstream writer.
const CHARS_PER_CHUNK: usize = 4096;
const CHUNK_SIZE: usize = (CHARS_PER_CHUNK / 4) * 3;

/// A cell whose symbol carries an escape rather than text: the diff must advance one column, not
/// measure the base64 as printable width.
const UNIT_WIDTH: CellDiffOption =
    CellDiffOption::ForcedWidth(std::num::NonZeroU16::new(1).unwrap());

/// Sentinel code points carry the atom's ordinal, so the painter can name the atom it found
/// without knowing the source's width.  Private use, one cell wide, and invisible in practice:
/// the placement's erase sweep removes it, and a refused placement overwrites it with the source.
const SENTINEL_BASE: u32 = 0xE000;
const SENTINEL_COUNT: u32 = 6400;

/// One inline formula, in document order.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Atom {
    source: String,
    /// Cells the atom reserves — the formula's measured ink.  `None` when this formula is *not*
    /// painted this build (it holds the cursor, RaTeX refused it, or it is too wide for the
    /// viewport): the ordinal still has to be spent, or every later atom would name the wrong one.
    cells: Option<u16>,
}

/// Whether the ordinal-th inline formula — whose source is the second argument — holds the
/// cursor.  Names the source so a disagreement between the parser's enumeration and our scan can
/// only mean "do not reveal", never "reveal a different formula".
pub type RevealFn = Box<dyn Fn(usize, &str) -> bool>;

/// What `EditorState` knows before a build and the renderer needs during it.
#[derive(Default)]
struct BuildInputs {
    /// The terminal's cell in pixels: the atom's width is measured at it.
    cell: Option<(u16, u16)>,
    revealed: Option<RevealFn>,
    /// The session's *Figures* consent, mirrored from `App::effective_diagrams_enabled`.  Fail
    /// closed: an atom rasterizes on the render path, with no decode dispatch to ask, so anything
    /// that did not explicitly grant consent keeps its literal source.
    consent: bool,
}

#[derive(Default)]
struct State {
    /// The one-time transmit per image id, until a placement has carried it.
    transmits: HashMap<u32, String>,
    /// Image ids the terminal has been sent and still holds.
    sent: HashSet<u32>,
    /// `(image id, placement id)` pairs the terminal is showing as of the last painted frame.
    live: HashSet<(u32, u32)>,
    /// Pairs painted during the frame being built.
    painted: HashSet<(u32, u32)>,
    /// Deletes owed to the terminal, waiting for a cell that will be emitted.
    deletes: Vec<(u32, u32)>,
    /// Rasterized formulas, keyed by everything that changes their pixels.
    bitmaps: HashMap<BitmapKey, image::DynamicImage>,
    /// Measured widths, keyed by `(source, cell, ratio)`.
    measured: HashMap<MeasureKey, Option<u16>>,
}

type BitmapKey = (String, u16, u16, u16, u32, [u8; 4]);
type MeasureKey = (String, (u16, u16), u32);

thread_local! {
    /// Atoms the last render pass emitted, in document order.
    static ATOMS: RefCell<Vec<Atom>> = const { RefCell::new(Vec::new()) };
    /// The build's inputs, installed by `EditorState` before it asks for a parse.
    static BUILD: RefCell<BuildInputs> = RefCell::new(BuildInputs::default());
    static STATE: RefCell<State> = RefCell::new(State::default());
}

thread_local! {
    /// Test seam for the gate: the real one reads the environment once (the paint path asks every
    /// frame), and a test cannot set the environment for a process that already read it.
    ///
    /// Thread-local, like [`PROTO_OVERRIDE`] and for the same reason: cargo runs a binary's tests
    /// on parallel threads, and two tests legitimately want different answers.  [`reset`] clears it.
    /// 0 = defer to the environment, 1 = forced on, 2 = forced off.
    static GATE_OVERRIDE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// Whether the spike is enabled.
///
/// `EDAMAME_INLINE_MATH` wins when it is set (`0` / `off` / empty disables, anything else
/// enables).  Unset, the spike asks the terminal instead of the user: it paints only where the
/// kitty protocol's *direct placement* is known to work, which today means WezTerm.
///
/// That default is deliberately conservative rather than convenient.  On a terminal that cannot
/// place an already-transmitted image the erase sweep still runs — it is part of the placement
/// symbol — so the formula's `$x$` disappears with nothing drawn in its place: a hole in the
/// prose, strictly worse than the literal source.  "Nobody asked for images" must therefore mean
/// *off* on such a terminal, and the capability check is the only thing that can tell them apart.
fn enabled() -> bool {
    match GATE_OVERRIDE.with(std::cell::Cell::get) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    // The environment is read once - it cannot change - but the support flag is read **live**.
    // The editor asks this question while it is being *built*, which is before the application has
    // looked at the terminal; caching the first (still false) answer here silently disabled the
    // whole feature for the session, on every terminal.
    static FROM_ENV: std::sync::LazyLock<Option<bool>> = std::sync::LazyLock::new(|| {
        std::env::var("EDAMAME_INLINE_MATH")
            .ok()
            .map(|v| v != "0" && !v.is_empty() && v != "off")
    });
    match *FROM_ENV {
        Some(on) => on,
        None => TERMINAL_SUPPORT.load(std::sync::atomic::Ordering::Relaxed) == 1,
    }
}

/// What the application decided about this terminal, before the first frame.
///
/// The spike deliberately does **not** probe the environment for itself: `WT_SESSION` is set in
/// every session Windows Terminal hosts, so a `cargo test` run inside one would silently take the
/// image path and change what the renderer snapshots see.  [`set_terminal_support`] is called once,
/// by the layer that knows the terminal.
static TERMINAL_SUPPORT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Tell the spike whether this terminal is one it can drive; [`detect_terminal_support`] is the
/// usual argument, from the application's terminal setup.
pub fn set_terminal_support(on: bool) {
    TERMINAL_SUPPORT.store(u8::from(on), std::sync::atomic::Ordering::Relaxed);
}

/// Which protocol this process's terminal speaks, or `None` for one the spike has no emitter for
/// (which keeps it off).
///
/// **WezTerm wins every tie, and that ordering is load-bearing.**  Windows Terminal is the *host* on
/// this platform - every shell started from a WT tab carries `WT_SESSION`, and so does anything
/// those shells start, a WezTerm included.  `TERM_PROGRAM` does not help: it is inherited the same
/// way, so a WezTerm launched from a WT tab still sees the host's `TERM_PROGRAM`, and trusting it
/// first sent WezTerm down the sixel path, where the kitty placement its terminal does honour is
/// never emitted.  That is a blank atom, indistinguishable from "the feature is off".
///
/// WezTerm's own markers are authoritative inside WezTerm's child tree, so they decide first.  The
/// reverse leak (a Windows Terminal started from a WezTerm pane) resolves to kitty and renders
/// nothing; that is the rarer arrangement and the one to fix by probing the terminal rather than by
/// reading the environment.
///
/// WezTerm's marker is also the one `terminal::capabilities` trusts for its truecolor guess; a
/// terminal that answers the kitty probe *and* supports placeholders (kitty, Ghostty) would need the
/// emitter's `U=1` sibling instead.  Windows Terminal renders through **sixel**: it has no kitty
/// graphics, its sixel pixels live in the text row itself (`ImageSlice`), and any text written over
/// those cells erases them - so the reveal is the very "rewrite the source" the kitty path has to
/// emulate with an explicit delete.
fn protocol_from(get: impl Fn(&str) -> Option<String>) -> Option<Protocol> {
    let is = |key: &str, want: &str| get(key).as_deref() == Some(want);
    if get("WEZTERM_PANE").is_some() || is("TERM_PROGRAM", "WezTerm") {
        Some(Protocol::Kitty)
    } else if get("WT_SESSION").is_some() || is("TERM_PROGRAM", "Windows_Terminal") {
        Some(Protocol::Sixel)
    } else {
        None
    }
}

/// The environment's verdict for this process.  [`detect_terminal_support`] is the yes/no form the
/// application calls.
fn terminal_protocol() -> Option<Protocol> {
    protocol_from(|key| std::env::var(key).ok().filter(|v| !v.is_empty()))
}

/// Whether this terminal speaks a protocol the spike implements (WezTerm's kitty direct placement,
/// or Windows Terminal's sixel).
pub fn detect_terminal_support() -> bool {
    terminal_protocol().is_some()
}

/// Which image protocol the paint path emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    /// WezTerm: one transmit per image id, one `a=p` placement per frame, explicit deletes.
    Kitty,
    /// Windows Terminal: a self-contained sixel payload per atom per frame - no image ids, no
    /// transmit, no deletes, because text written over those cells erases the image by itself.
    Sixel,
}

thread_local! {
    /// Test seam for [`protocol`]: 0 = decide from the environment, 1 = kitty, 2 = sixel.
    ///
    /// Thread-local rather than the process-wide atomic [`GATE_OVERRIDE`] uses, because two tests
    /// can legitimately want *different* protocols - and cargo runs a binary's tests on parallel
    /// threads, where a shared cell would let one test's `reset` clear another test's choice
    /// mid-assertion.
    static PROTO_OVERRIDE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

fn protocol() -> Protocol {
    match PROTO_OVERRIDE.with(std::cell::Cell::get) {
        1 => return Protocol::Kitty,
        2 => return Protocol::Sixel,
        _ => {}
    }
    static CHOSEN: std::sync::LazyLock<Option<Protocol>> =
        std::sync::LazyLock::new(terminal_protocol);
    CHOSEN.unwrap_or(Protocol::Kitty)
}

#[cfg(test)]
fn force_protocol(p: Protocol) {
    PROTO_OVERRIDE.with(|o| {
        o.set(match p {
            Protocol::Kitty => 1,
            Protocol::Sixel => 2,
        })
    });
}

/// Test seam, crate-visible so a caller elsewhere can pin "what the spike does while it is off"
/// (e.g. `markdown::render_cache`'s bypass).
#[cfg(test)]
pub(crate) fn force_enabled(on: bool) {
    GATE_OVERRIDE.with(|o| o.set(if on { 1 } else { 2 }));
}

/// Where the text baseline sits inside the formula's cell row.
///
/// The default is *measured*, not chosen: WezTerm's default font on this machine puts the text
/// baseline 2 px below `0.8 × cell_h` (cell ≈ 40 px), so 0.8 renders every formula 2 px low and
/// 0.74 lands its ink bottom on the text's, pixel for pixel (two frame captures compared at the
/// two ratios).  The terminal does not expose the font's ascent,
/// so this ratio — not the formula's size — is the one calibration knob a real implementation
/// needs; `EDAMAME_INLINE_MATH_RATIO` exists to re-measure it per font.
fn baseline_ratio() -> f32 {
    static RATIO: std::sync::LazyLock<f32> = std::sync::LazyLock::new(|| {
        std::env::var("EDAMAME_INLINE_MATH_RATIO")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.74)
            .clamp(0.4, 0.95)
    });
    *RATIO
}

/// Drop the previous build's atoms.  Called once per parse, before any block is rendered — a
/// stale table would paint formulas at another document's offsets.
pub fn begin_build() {
    ATOMS.with(|a| a.borrow_mut().clear());
}

/// Install the build's inputs.  `EditorState` calls this immediately before asking for a parse;
/// `cell` is `None` when the spike is off or the terminal reports no cell size, which turns the
/// whole render path back into the literal-source one.  `consent` is the session's *Figures*
/// decision — see [`is_painting`].
pub fn set_build_inputs(cell: Option<(u16, u16)>, revealed: Option<RevealFn>, consent: bool) {
    BUILD.with(|b| {
        *b.borrow_mut() = BuildInputs {
            cell,
            revealed,
            consent,
        };
    });
}

/// The cell size this build measures atoms at, or `None` when the spike is not laying out.
pub fn cell_size() -> Option<(u16, u16)> {
    if !is_painting() {
        return None;
    }
    BUILD.with(|b| b.borrow().cell)
}

/// Whether the spike is switched on at all — the gate alone, without the per-build inputs.
/// `EditorState` asks before paying for [`scan_inline_math`].  This answers *capability* (the
/// terminal plus `EDAMAME_INLINE_MATH`); whether pictures may actually be drawn also needs the
/// session's *Figures* consent, which is [`is_painting`].
pub fn is_active() -> bool {
    enabled()
}

/// Whether this build may draw atoms: the gate *and* the session's *Figures* consent.  An atom
/// rasterizes on the render path — its cache is a side effect of rendering — so unlike a block
/// figure it never passes through the decode dispatch where `config.figures.enabled` is enforced.
/// The App mirrors `effective_diagrams_enabled` into the build inputs for exactly that reason; with
/// no consent recorded, this is `false`.
pub fn is_painting() -> bool {
    enabled() && BUILD.with(|b| b.borrow().consent)
}

/// Whether the ordinal-th formula, whose source is `source`, holds the cursor.
pub fn is_revealed(ordinal: usize, source: &str) -> bool {
    BUILD.with(|b| {
        b.borrow()
            .revealed
            .as_ref()
            .is_some_and(|f| f(ordinal, source))
    })
}

/// Cells `source` needs at `cell`: the raster's ink width, measured once per `(source, cell,
/// ratio)`.  `None` when RaTeX refuses the formula — the caller then keeps the literal source.
pub fn measure(source: &str, cell: (u16, u16)) -> Option<u16> {
    let key: MeasureKey = (source.to_owned(), cell, baseline_ratio().to_bits());
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(v) = s.measured.get(&key) {
            return *v;
        }
        let v =
            crate::diagram::math::inline_latex_width_cells(source, Some(cell), baseline_ratio());
        // The working set is the document's distinct formulas; the cap is only so a pathological
        // document cannot grow it without bound.
        if s.measured.len() > 512 {
            s.measured.clear();
        }
        s.measured.insert(key, v);
        v
    })
}

/// Record one inline formula.  Called by the renderer's `Inline::Math` arm for *every* inline
/// formula it renders, painted or not, so ordinals line up with the document.
pub fn register(source: &str, cells: Option<u16>) {
    ATOMS.with(|a| {
        a.borrow_mut().push(Atom {
            source: source.to_owned(),
            cells,
        })
    });
}

/// Number of atoms the last build registered.  This is also the ordinal the renderer is about to
/// hand to the next formula.
pub fn atom_count() -> usize {
    ATOMS.with(|a| a.borrow().len())
}

/// Forget every placement, bitmap and measurement.
#[cfg(test)]
pub(crate) fn reset() {
    PROTO_OVERRIDE.with(|o| o.set(0));
    GATE_OVERRIDE.with(|o| o.set(0));
    STATE.with(|s| *s.borrow_mut() = State::default());
    BUILD.with(|b| *b.borrow_mut() = BuildInputs::default());
    begin_build();
}

/// The span an inline formula renders as when the spike will paint it: `cells` single-width
/// characters the painter can find again.
///
/// The sentinel — a private-use code point carrying the atom's ordinal — is placed **last**.
/// `line_render::is_break_after` treats any non-alphanumeric character as a break opportunity, so
/// a sentinel in the first cell would let the wrapper split the atom open; trailing it leaves the
/// run of `U+00A0` (never a break point) unbreakable and allows a break only after the whole atom.
pub fn atom_span(cells: u16, ordinal: usize, style: Style) -> ratatui::text::Span<'static> {
    let cells = cells.max(1);
    let mut text = String::with_capacity(usize::from(cells));
    for _ in 1..cells {
        text.push('\u{00A0}');
    }
    text.push(sentinel(ordinal));
    ratatui::text::Span::styled(text, style)
}

fn sentinel(ordinal: usize) -> char {
    let offset = (ordinal as u32) % SENTINEL_COUNT;
    char::from_u32(SENTINEL_BASE + offset).unwrap_or('\u{E000}')
}

/// The ordinal a sentinel character carries.
fn ordinal_of(c: char) -> Option<usize> {
    // `checked_sub` rather than a range test plus `then_some`: `then_some`'s argument is evaluated
    // eagerly, so the subtraction would run for every ordinary character too (and underflow in a
    // debug build — which is how this was found).
    let offset = (c as u32).checked_sub(SENTINEL_BASE)?;
    (offset < SENTINEL_COUNT).then_some(offset as usize)
}

/// The byte range of the source line containing `byte`, its trailing newline excluded.
///
/// This is the unit the inline reveal works in: the atom's image is hidden for *every* formula on
/// the cursor's line, not only the one the cursor sits inside.  A terminal cannot place a cursor
/// *between* two cells of one atom, so a line-scoped reveal is the only form a user can reliably
/// aim at - and it is also what the raw-reveal overlay for the cursor's line already shows, so the
/// two layers agree instead of one painting an image over what the other drew as source.
pub fn line_bounds(text: &str, byte: usize) -> Range<usize> {
    let byte = byte.min(text.len());
    let start = text[..byte].rfind('\n').map_or(0, |i| i + 1);
    let end = text[byte..].find('\n').map_or(text.len(), |i| byte + i);
    start..end
}

/// Every inline `$…$` span in `text`, in document order, as `(byte range of the source, source)`.
///
/// This is how the *reveal* names formulas: the renderer enumerates them through pulldown-cmark,
/// and this scan is what lets `EditorState` answer "is this formula on the cursor's line?" without
/// a second parse.  It mirrors the two rules that matter in prose — an opening `$` must be followed
/// by non-whitespace, a closing one preceded by it, delimiters cannot be escaped — and skips
/// `$$…$$` outright.  It is deliberately allowed to disagree with the parser: the caller compares
/// each span's text against the formula it is being asked about, so a disagreement can only mean
/// "do not reveal".
pub fn scan_inline_math(text: &str) -> Vec<(Range<usize>, String)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut k = 0usize;
    while k < chars.len() {
        match chars[k].1 {
            '\\' => k += 2,
            '$' => {
                if chars.get(k + 1).is_some_and(|(_, c)| *c == '$') {
                    // Display math: skip past its closer.
                    let mut m = k + 2;
                    while m + 1 < chars.len() && !(chars[m].1 == '$' && chars[m + 1].1 == '$') {
                        m += 1;
                    }
                    k = if m + 1 < chars.len() {
                        m + 2
                    } else {
                        chars.len()
                    };
                    continue;
                }
                let Some(&(_, after_open)) = chars.get(k + 1) else {
                    break;
                };
                if after_open.is_whitespace() {
                    k += 1;
                    continue;
                }
                let mut m = k + 1;
                let mut close = None;
                while m < chars.len() {
                    match chars[m].1 {
                        '\n' => break,
                        '\\' => m += 2,
                        '$' if !chars[m - 1].1.is_whitespace()
                            && !chars.get(m + 1).is_some_and(|(_, c)| *c == '$') =>
                        {
                            close = Some(m);
                            break;
                        }
                        _ => m += 1,
                    }
                }
                match close {
                    Some(m) => {
                        let (start, end) = (chars[k + 1].0, chars[m].0);
                        out.push((start..end, text[start..end].to_owned()));
                        k = m + 1;
                    }
                    None => k += 1,
                }
            }
            _ => k += 1,
        }
    }
    out
}

/// Paint every visible inline formula.  No-op unless the spike is on and the terminal reports a
/// cell pixel size; the caller passes the document area so the escapes carry absolute coordinates.
///
/// The cell size comes from `EditorState::image_font_size` — the *same* field the layout measured
/// the atoms at.  Reading it from the capabilities instead would let the two disagree (the probe
/// can fail while the field keeps its fallback), and a painter that measures differently from the
/// layout either paints nothing or paints the wrong width.
pub fn paint(state: &mut EditorState, area: Rect, buf: &mut TuiBuf) {
    STATE.with(|s| paint_inner(&mut s.borrow_mut(), state, area, buf));
}

fn paint_inner(st: &mut State, state: &mut EditorState, area: Rect, buf: &mut TuiBuf) {
    st.painted.clear();
    let (cell_w, cell_h) = (
        state.image_font_size.0.max(1),
        state.image_font_size.1.max(1),
    );
    if !enabled() || area.width == 0 || area.height == 0 {
        reconcile(st, buf, area);
        return;
    }
    let atoms: Vec<Atom> = ATOMS.with(|a| a.borrow().clone());
    if atoms.is_empty() {
        reconcile(st, buf, area);
        return;
    }

    let parsed = &state.parsed;
    let width = area.width as usize;
    // The same source of truth the view and the reveal overlay use: a revealed block replaces its
    // rendered rows with its raw ones, so the number of rows on screen is not the number of
    // rendered rows.
    let effective = state.effective_rows(width);
    let ratio = baseline_ratio();
    let text_rgb = crate::ui::dim::color_to_rgb(state.theme().palette.text)
        .map(|[r, g, b]| [r, g, b, 255])
        .unwrap_or([0xcc, 0xcc, 0xcc, 255]);
    // The page colour the formula is flattened onto: `palette.bg` is what the theme paints every
    // cell with, so an opaque atom sits on exactly the same pixels a transparent one would.
    let page_rgb = crate::ui::dim::color_to_rgb(state.theme().palette.bg)
        .map(|[r, g, b]| [r, g, b, 255])
        .unwrap_or([0x1a, 0x1a, 0x1a, 255]);
    let source_style = state.theme().normal.patch(state.theme().code_span);

    // Walk the visible rendered lines, from the first one the viewport starts inside of.
    let (mut idx, first_skip) = state.rendered_line_at_visual_row(state.scroll, width);
    let mut skip_rows = first_skip;
    let mut screen_row = 0usize;
    while screen_row < area.height as usize && idx < parsed.lines.len() {
        let rows = parsed.visual_rows_for_line_at(idx, width).max(1);
        let line = &parsed.lines[idx];
        let chars: Vec<(char, ratatui::style::Style)> = line
            .spans
            .iter()
            .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
            .collect();
        let indent = crate::ui::line_render::compute_hanging_indent(line);
        let wrap_rows = crate::ui::line_render::visual_rows_of_chars(&chars, width, indent);

        for (ordinal, char_idx, atom) in find_atoms(&chars, &atoms) {
            let Some(cells) = atom.cells else {
                continue;
            };
            let (sub, _) = crate::ui::line_render::sub_line_of_col(&wrap_rows, char_idx);
            if sub < skip_rows {
                continue; // On a row scrolled off the top.
            }
            let (row_start, _, _) = wrap_rows[sub];
            let row_indent = if sub == 0 { 0 } else { indent };
            // Cells, not characters: with a wide glyph (CJK, emoji) earlier on the row the two
            // diverge, and a char-index column puts the image that many cells to the *left* of the
            // text it belongs to.  `char_cells` is the same unit everything else measures in.
            let col = crate::ui::line_render::cell_col_at_char_idx(
                chars.iter().map(|(c, _)| *c).skip(row_start),
                char_idx.saturating_sub(row_start),
                row_indent,
            );
            let y = screen_row + (sub - skip_rows);
            if y >= area.height as usize {
                continue;
            }
            let dst = Rect {
                x: area.x + col as u16,
                y: area.y + y as u16,
                width: cells,
                height: 1,
            };
            // A row showing the *raw* form of a revealed block is source text, not rendered cells:
            // this atom's span is not on that row at all, and painting over it would put an image on
            // top of the text the reader is editing.  The reveal is line-scoped, so the row is the
            // only place this can be asked - and `EffectiveRows` is what the view itself asks.
            if matches!(
                effective.line_at_visual_row(state.scroll + y),
                crate::editor::effective_rows::RowHit::Raw { .. }
            ) {
                paint_source_fallback(buf, dst, &atom.source, source_style);
                continue;
            }
            // Windows Terminal scrolls the whole page when a sixel needs room below it (measured:
            // an atom on the last row pushed everything up one row), so that row keeps its source.
            if protocol() == Protocol::Sixel && dst.y as usize + 1 >= buf.area.height as usize {
                paint_source_fallback(buf, dst, &atom.source, source_style);
                continue;
            }
            // A wrapped atom (no break opportunity before it, so the row split it) or one running
            // past the viewport: leave the source readable rather than half an image.
            let fits = dst.x as usize + dst.width as usize <= area.x as usize + area.width as usize;
            if dst.width == 0 || !fits {
                paint_source_fallback(buf, dst, &atom.source, source_style);
                continue;
            }

            let key: BitmapKey = (
                atom.source.clone(),
                cells,
                cell_w,
                cell_h,
                ratio.to_bits(),
                page_rgb,
            );
            let Some(bitmap) = bitmap_for(
                st,
                &key,
                &atom,
                cells,
                (cell_w, cell_h),
                text_rgb,
                page_rgb,
                ratio,
            ) else {
                paint_source_fallback(buf, dst, &atom.source, source_style);
                continue;
            };

            let geometry = Geometry {
                cells: (cells, 1),
                font: (cell_w, cell_h),
            };
            let id = image_id(&atom.source, geometry);
            if protocol() == Protocol::Kitty
                && !st.sent.contains(&id)
                && !st.transmits.contains_key(&id)
            {
                st.transmits.insert(id, transmit_payload(id, &bitmap));
            }
            // The carrier cell has to exist before the payload is taken: taking it and then
            // failing to write the symbol would drop the transmit for good.
            let Some(cell) = buf.cell_mut((dst.x, dst.y)) else {
                continue;
            };
            let placement_id = (ordinal as u32).saturating_add(1);
            let payload = st.transmits.remove(&id);
            if payload.is_some() {
                st.sent.insert(id);
            }
            let symbol = match protocol() {
                Protocol::Kitty => {
                    placement_symbol(id, placement_id, geometry, 0, dst, payload.as_deref())
                }
                Protocol::Sixel => sixel_symbol(&bitmap, dst, text_rgb, page_rgb),
            };
            cell.set_symbol(&symbol).set_diff_option(UNIT_WIDTH);
            // The atom's *other* cells are skipped: the erase sweep the symbol begins with has
            // already cleared them on the terminal, and emitting our blanks would paint over the
            // image this placement draws.  The carrier itself must **not** be skipped — a skipped
            // cell is dropped from the diff, so the escape would never leave the process.
            for col in 1..dst.width {
                if let Some(cell) = buf.cell_mut((dst.x + col, dst.y)) {
                    cell.set_diff_option(CellDiffOption::Skip);
                }
            }
            if protocol() == Protocol::Kitty {
                st.painted.insert((id, placement_id));
            }
        }

        let consumed = rows.saturating_sub(skip_rows);
        screen_row += consumed;
        skip_rows = 0;
        idx += 1;
    }

    reconcile(st, buf, area);
}

/// Write an atom's source into its cells — the fallback when the image cannot be painted there.
/// Without it the reader would see the `U+00A0`/sentinel cells, i.e. nothing.
fn paint_source_fallback(buf: &mut TuiBuf, dst: Rect, source: &str, style: Style) {
    // The source is wider than the atom's ink — that is *why* the atom is narrow — and a truncated
    // formula reads as garbage (`\frac{1}{2}` came out as `\fra` on Windows Terminal's last row).
    // Write it whole, up to the frame's right edge.  Editing the ratatui buffer rather than the
    // terminal is what makes the spill safe: the neighbours it covers are restored by the next
    // frame's diff.
    let frame_right = buf.area.x.saturating_add(buf.area.width);
    let last = frame_right.min(dst.x.saturating_add(source.chars().count() as u16));
    for (x, c) in (dst.x..last).zip(source.chars()) {
        if let Some(cell) = buf.cell_mut((x, dst.y)) {
            let _ = cell.set_char(c);
            cell.set_style(style);
            cell.set_diff_option(CellDiffOption::None);
        }
    }
}

/// The rasterized formula for `key`, rendering it on first use.  `None` when RaTeX refuses the
/// source — the atom then keeps its source text, like a failed block diagram keeps its fence.
#[allow(clippy::too_many_arguments)]
fn bitmap_for(
    st: &mut State,
    key: &BitmapKey,
    atom: &Atom,
    cells: u16,
    cell: (u16, u16),
    fg: [u8; 4],
    bg: [u8; 4],
    ratio: f32,
) -> Option<image::DynamicImage> {
    if let Some(b) = st.bitmaps.get(key) {
        return Some(b.clone());
    }
    match crate::diagram::math::render_inline_latex(&atom.source, cells, Some(cell), fg, bg, ratio)
    {
        Ok(b) => {
            st.bitmaps.insert(key.clone(), b.clone());
            Some(b)
        }
        Err(e) => {
            tracing::debug!(target: "image", %e, "inline math render failed");
            None
        }
    }
}

/// Remove placements the frame did not paint — a scrolled-away or revealed atom, or one whose
/// bitmap changed geometry.
fn reconcile(st: &mut State, buf: &mut TuiBuf, area: Rect) {
    for pair in st.live.difference(&st.painted) {
        st.deletes.push(*pair);
    }
    st.live = st.painted.clone();
    if st.deletes.is_empty() {
        return;
    }
    // A delete is an escape with no visual effect, so it needs a cell that reaches the terminal
    // this frame: the document area's first cell is the one such a frame always has.
    let Some(cell) = buf.cell_mut((area.x, area.y)) else {
        st.deletes.clear();
        return;
    };
    let mut symbol = cell.symbol().to_owned();
    for (id, placement_id) in st.deletes.drain(..) {
        let _ = write!(symbol, "\x1b_Gq=2,i={id},p={placement_id},a=d,d=i\x1b\\");
    }
    cell.set_symbol(&symbol).set_diff_option(UNIT_WIDTH);
}

/// Every atom sentinel on the line, as `(ordinal, char offset of the atom's first cell, atom)`.
fn find_atoms(
    chars: &[(char, ratatui::style::Style)],
    atoms: &[Atom],
) -> Vec<(usize, usize, Atom)> {
    let mut out = Vec::new();
    for (idx, (c, _)) in chars.iter().enumerate() {
        let Some(ordinal) = ordinal_of(*c) else {
            continue;
        };
        let Some(atom) = atoms.get(ordinal) else {
            continue;
        };
        let Some(cells) = atom.cells else {
            continue;
        };
        let first = idx.saturating_sub(usize::from(cells.max(1)) - 1);
        out.push((ordinal, first, atom.clone()));
    }
    out
}

/// One image's geometry at one cell size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    cells: (u16, u16),
    font: (u16, u16),
}

impl Geometry {
    fn pixels(self) -> (u32, u32) {
        (
            u32::from(self.cells.0) * u32::from(self.font.0),
            u32::from(self.cells.1) * u32::from(self.font.1),
        )
    }
}

/// Stable per-`(source, geometry)` image id: the geometry is part of the hash because an id names
/// one stored bitmap, and zero is avoided because it means "no id" to the placement path.
fn image_id(source: &str, geometry: Geometry) -> u32 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"inline-math");
    hasher.update(source.as_bytes());
    for v in [
        geometry.cells.0,
        geometry.cells.1,
        geometry.font.0,
        geometry.font.1,
    ] {
        hasher.update(v.to_le_bytes());
    }
    let digest = hasher.finalize();
    let mut id = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]);
    if id == 0 {
        id = 1;
    }
    id
}

/// The one-time transmit: the whole bitmap as raw RGBA (`f=32,t=d`), in base64 chunks.
fn transmit_payload(id: u32, image: &image::DynamicImage) -> String {
    let rgba = image.to_rgba8();
    let bytes = rgba.as_raw();
    let (width, height) = (image.width(), image.height());
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 96);
    for (i, chunk) in bytes.chunks(CHUNK_SIZE).enumerate() {
        out.push_str("\x1b_Gq=2,");
        if i == 0 {
            let _ = write!(out, "i={id},a=t,f=32,t=d,s={width},v={height},");
        }
        let more = u8::from((i + 1) * CHUNK_SIZE < bytes.len());
        let _ = write!(out, "m={more};");
        base64::Engine::encode_string(&base64::engine::general_purpose::STANDARD, chunk, &mut out);
        out.push_str("\x1b\\");
    }
    out
}

/// The cell symbol that places the atom's bitmap at `dst`.
///
/// It begins by erasing the atom's cells — the source text and the sentinel draw *above* the
/// image, and a buffer cell marked `Skip` cannot remove them — and ends one cell right of `dst`'s
/// origin, which is where `ratatui-crossterm` believes the cell cursor is.
///
/// `sub_cell_y` is the kitty `Y=` pixel offset within the cell; the padding variant leaves it 0,
/// and the probe measured the terminal honouring it 1:1 in device pixels.
fn placement_symbol(
    id: u32,
    placement_id: u32,
    geometry: Geometry,
    sub_cell_y: u16,
    dst: Rect,
    transmit: Option<&str>,
) -> String {
    let (pixels_w, _) = geometry.pixels();
    let src_h = u32::from(dst.height) * u32::from(geometry.font.1);
    let mut out =
        String::with_capacity(transmit.map_or(0, str::len) + usize::from(dst.height) * 16 + 96);
    for row in 0..dst.height {
        let _ = write!(
            out,
            "\x1b[{};{}H\x1b[{}X",
            dst.y + row + 1,
            dst.x + 1,
            dst.width
        );
    }
    if let Some(transmit) = transmit {
        out.push_str(transmit);
    }
    let _ = write!(
        out,
        "\x1b[{};{}H\x1b_Gq=2,i={id},p={placement_id},a=p,x=0,y=0,w={pixels_w},h={src_h},c={},r={}",
        dst.y + 1,
        dst.x + 1,
        dst.width,
        dst.height
    );
    if sub_cell_y > 0 {
        let _ = write!(out, ",Y={sub_cell_y}");
    }
    out.push_str(",C=1\x1b\\");
    let _ = write!(out, "\x1b[{};{}H", dst.y + 1, dst.x + 2);
    out
}

/// The cell symbol that paints a sixel atom: the same erase sweep the kitty placement begins with,
/// the image itself, then the cursor parked one cell right of the atom's origin.
///
/// Both conventions are shared with [`placement_symbol`] on purpose.  The erase sweep (`ECH` over the
/// atom's cells) is what removes the source glyphs *and* the `code_span` chip underneath them - with
/// a transparent-background sixel the cells show through wherever the formula has no ink, so the chip
/// has to go.  The trailing position is not cosmetic either: Windows Terminal's own sixel bookkeeping
/// moves the text cursor to the image's last row when the image's sixel height exceeds the cell
/// height, while ratatui-crossterm believes the cursor sits at the cell after this one.
fn sixel_symbol(bitmap: &image::DynamicImage, dst: Rect, ink: [u8; 4], page: [u8; 4]) -> String {
    let mut out = String::new();
    for row in 0..dst.height {
        let _ = write!(
            out,
            "\x1b[{};{}H\x1b[{}X",
            dst.y + row + 1,
            dst.x + 1,
            dst.width
        );
    }
    out.push_str(&crate::image::sixel::encode(
        bitmap,
        dst.width,
        [ink[0], ink[1], ink[2]],
        [page[0], page[1], page[2]],
    ));
    let _ = write!(out, "\x1b[{};{}H", dst.y + 1, dst.x + 2);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::Buffer;
    use crate::editor::Mode;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn state(src: &str) -> EditorState {
        EditorState::new(Buffer::from_str(src), theme())
    }

    /// The atom's span is exactly `cells` single-width characters, and its last one carries the
    /// ordinal — the painter's only handle on the atom.
    #[test]
    fn an_atom_span_is_its_measured_width_with_a_trailing_sentinel() {
        let span = atom_span(3, 7, Style::default());
        let text: String = span.content.to_string();
        assert_eq!(text.chars().count(), 3);
        let last = text.chars().last().expect("non-empty");
        assert_eq!(ordinal_of(last), Some(7));
        assert!(text.chars().take(2).all(|c| c == '\u{00A0}'));
        // Why the sentinel is *last* is a layout claim, pinned next to the rule it depends on:
        // `line_render::an_atom_run_breaks_only_after_its_trailing_sentinel`.
    }

    /// The reveal's scan: sources in document order, with display math and escaped dollars left
    /// out.  The caller compares the text against the formula it asked about, so a disagreement
    /// here can only mean "do not reveal".
    #[test]
    fn the_scan_finds_inline_sources_in_order() {
        let text = "Example: input $X$ and $Y_1$, then $\\alpha$.\n\n$$\nX = 1\n$$\n\ncost \\$5\n";
        let spans: Vec<String> = scan_inline_math(text)
            .into_iter()
            .map(|(_, source)| source)
            .collect();
        assert_eq!(spans, vec!["X", "Y_1", "\\alpha"]);
        // A lone `$` is not an opener, and `$$…$$` is display math.
        assert!(scan_inline_math("$ 5 and $10\n").is_empty());
    }

    /// A session that declined *Figures* must not paint atoms even where the terminal could: the
    /// atom path rasterizes on the render path, so this is the only place its consent is enforced.
    #[test]
    fn a_declined_figures_setting_never_reaches_the_paint_path() {
        reset();
        force_enabled(true);
        set_build_inputs(Some((10, 20)), None, false);
        assert!(is_active(), "capability is unchanged by a consent decision");
        assert!(!is_painting(), "no consent, no painting");
        assert!(
            cell_size().is_none(),
            "the renderer's `Inline::Math` arm takes its literal-source path"
        );
        assert!(
            measure("Y_1", (10, 20)).is_some(),
            "measurement itself is not consent-gated; only painting is"
        );

        set_build_inputs(Some((10, 20)), None, true);
        assert!(is_painting(), "consent reopens the path");
        assert_eq!(cell_size(), Some((10, 20)));
    }

    /// The width the renderer reserves is the *measured* one, so the text after a formula sits
    /// where the ink ends rather than where the source text would have ended.
    #[test]
    fn the_reserved_width_is_the_measured_ink() {
        reset();
        force_enabled(true);
        let st = state("GPU0 holds $Y_1$, GPU1 holds $Y_2$.\n");
        let atoms: Vec<Atom> = ATOMS.with(|a| a.borrow().clone());
        assert_eq!(atoms.len(), 2, "one atom per inline formula: {atoms:?}");
        for atom in &atoms {
            let cells = atom.cells.expect("painted");
            let source_cells = atom.source.chars().count() as u16 + 2;
            assert!(
                cells < source_cells,
                "{:?}: reserved {cells} cells must be narrower than the {source_cells} source characters",
                atom.source
            );
        }
        // And the rendered line is correspondingly shorter than it would be as source text.
        let line: String = st
            .parsed
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.to_string())
            .collect();
        assert!(
            line.contains('\u{E000}') || line.contains('\u{E001}'),
            "{line:?}"
        );

        // And the painter finds those sentinels: one placement per atom, each naming the atom's
        // own cells.
        let area = Rect::new(0, 0, 40, 4);
        let mut buf = TuiBuf::empty(area);
        let mut st = st;
        paint(&mut st, area, &mut buf);
        let placed: Vec<String> = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .filter_map(|(x, y)| {
                let s = buf.cell((x, y))?.symbol().to_owned();
                s.contains("a=p").then_some(s)
            })
            .collect();
        assert_eq!(placed.len(), 2, "one placement per atom: {placed:?}");
        for (symbol, atom) in placed.iter().zip(&atoms) {
            assert!(
                symbol.contains(&format!(",c={},r=1,", atom.cells.expect("painted"))),
                "{symbol:?}"
            );
        }
        reset();
    }

    /// The reveal is scoped to the cursor's *line*, not to the one formula the cursor sits inside.
    /// A terminal cannot aim a cursor between two cells of one atom, and edamame's raw-reveal
    /// overlay already draws the cursor's line as source - so hiding anything less than the whole
    /// line leaves an image painted over text that is being shown, which is how this was reported
    /// ("the SVG stays and a `$` appears beside it").  Rebuilding is what makes the two agree: the
    /// atoms' widths differ between the two forms, so the renderer has to run again.
    #[test]
    fn the_cursor_line_reveals_its_formulas_and_only_a_line_change_rebuilds() {
        reset();
        force_enabled(true);
        let mut st = state("$X$ then $Y_1$ here.\nplain $Z$ line.\nno formula here.\n");
        assert!(
            atom_cells().iter().all(|c| c.is_some()),
            "all three formulas paint to begin with: {:?}",
            atom_cells()
        );
        let text = st.buffer.contents();
        let at = |marker: &str| text[..text.find(marker).expect("marker")].chars().count();

        // The reported case: the cursor lands beside `$X$`, on its line but inside no atom - the
        // click path (a click maps to a cell) and the plain arrow-key path both land here.
        st.cursor.offset = at(" then");
        assert!(st.sync_inline_math_reveal(), "the move must rebuild");
        let cells = atom_cells();
        assert!(
            cells[..2].iter().all(Option::is_none),
            "both formulas of the cursor's line render as source: {cells:?}"
        );
        assert!(
            cells[2].is_some(),
            "the other line keeps its image: {cells:?}"
        );

        // A move inside that line changes nothing, so nothing rebuilds.
        st.cursor.offset = 0;
        assert!(!st.sync_inline_math_reveal(), "same line, no rebuild");

        // Onto the `$Z$` line: that one reveals, the first line's formulas take their images back.
        st.cursor.offset = at("plain");
        assert!(st.sync_inline_math_reveal(), "the line changed");
        let cells = atom_cells();
        assert!(
            cells[..2].iter().all(Option::is_some),
            "the first line's formulas are painted again: {cells:?}"
        );
        assert!(
            cells[2].is_none(),
            "the cursor's line shows source: {cells:?}"
        );

        // Onto a line with no formula at all: still a rebuild, but nothing reveals.
        st.cursor.offset = at("no formula");
        assert!(
            st.sync_inline_math_reveal(),
            "the revealed line went to none"
        );
        assert!(
            atom_cells().iter().all(|c| c.is_some()),
            "no formula on this line, so all keep their images: {:?}",
            atom_cells()
        );

        // And staying there costs nothing.
        st.cursor.offset = at("formula here");
        assert!(!st.sync_inline_math_reveal(), "both lines are formula-less");
        reset();
    }

    /// On Windows Terminal an atom's carrier cell holds a self-contained sixel payload instead of a
    /// kitty transmit/placement pair: no image id, nothing to delete - but the same erase sweep and
    /// trailing cursor position, because the erase is what clears the source glyphs and the chip, and
    /// WT's sixel bookkeeping moves the cursor off the cell ratatui expects.
    #[test]
    fn a_sixel_atom_erases_its_cells_and_sends_the_image_inline() {
        reset();
        force_enabled(true);
        force_protocol(Protocol::Sixel);
        let mut st = state("GPU0 holds $Y_1$.\n");
        let area = Rect::new(0, 0, 40, 3);
        let mut buf = TuiBuf::empty(area);
        paint(&mut st, area, &mut buf);
        let symbols: Vec<String> = (0..area.width)
            .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_owned()))
            .collect();
        let carrier = symbols
            .iter()
            .find(|s| s.contains("q\"1;1;"))
            .unwrap_or_else(|| panic!("no sixel payload in any cell: {symbols:?}"));
        let head = &carrier[..carrier.len().min(24)];
        assert!(carrier.contains("\x1bP0;1;0q"), "transparent DCS: {head}");
        assert!(
            !carrier.contains("\x1b_G"),
            "no kitty escape on the sixel path"
        );
        assert!(
            carrier.starts_with("\x1b[1;"),
            "the erase sweep comes first: {head}"
        );
        assert!(
            carrier.matches("\x1b[1;").count() >= 2,
            "erase sweep then the cursor parked for ratatui: {head}"
        );
        assert_eq!(
            symbols.iter().filter(|s| s.contains("q\"1;1;")).count(),
            1,
            "one payload per atom"
        );
        reset();
    }

    /// The last visible row keeps its source: a sixel there makes WT scroll the whole page (measured:
    /// an anchor on row 1 was pushed off the top), and a scrolled page is worse than a literal
    /// formula.
    #[test]
    fn an_atom_on_the_last_row_stays_literal() {
        reset();
        force_enabled(true);
        force_protocol(Protocol::Sixel);
        let mut st = state("one\ntwo\nthree\nholds $Y_1$.\n");
        let area = Rect::new(0, 0, 40, 4);
        let mut buf = TuiBuf::empty(area);
        paint(&mut st, area, &mut buf);
        let last: Vec<String> = (0..area.width)
            .filter_map(|x| buf.cell((x, 3)).map(|c| c.symbol().to_owned()))
            .collect();
        assert!(
            !last.iter().any(|s| s.contains("\x1bP")),
            "no sixel on the last row: {last:?}"
        );
        assert!(
            last.iter().any(|s| s == "Y") && last.iter().any(|s| s == "_"),
            "the source is painted instead: {last:?}"
        );
        reset();
    }

    /// WezTerm wins every tie.  Its markers are the authoritative ones inside WezTerm's child tree,
    /// while `WT_SESSION` - and `TERM_PROGRAM` with it - leaks down from the host: a WezTerm started
    /// from a Windows Terminal tab carries both, and resolving that to sixel produced a blank atom
    /// (measured twice: once with the sixel path, once by trusting the inherited `TERM_PROGRAM`).
    #[test]
    fn wezterm_markers_beat_the_inherited_windows_terminal_ones() {
        let wezterm_inside_wt = |k: &str| match k {
            "WEZTERM_PANE" => Some("0".to_string()),
            "TERM_PROGRAM" => Some("Windows_Terminal".to_string()),
            "WT_SESSION" => Some("leaked from the host".to_string()),
            _ => None,
        };
        assert_eq!(protocol_from(wezterm_inside_wt), Some(Protocol::Kitty));

        // The reverse leak is the known limitation: WT started from a WezTerm pane reads as kitty,
        // and renders nothing.  Pinned so the trade-off is a decision, not an accident.
        let wt_inside_wezterm = |k: &str| match k {
            "WEZTERM_PANE" => Some("leaked from the host".to_string()),
            "WT_SESSION" => Some("0".to_string()),
            _ => None,
        };
        assert_eq!(protocol_from(wt_inside_wezterm), Some(Protocol::Kitty));

        // Each terminal alone.
        assert_eq!(
            protocol_from(|k| (k == "WT_SESSION").then(|| "0".to_string())),
            Some(Protocol::Sixel)
        );
        assert_eq!(
            protocol_from(|k| (k == "TERM_PROGRAM").then(|| "WezTerm".to_string())),
            Some(Protocol::Kitty)
        );
        assert_eq!(
            protocol_from(|_| None),
            None,
            "a terminal the spike cannot drive keeps it off"
        );
    }

    /// F1: the unit trap this feature shares with the CJK table fixes - a *character* index is not
    /// a *cell* column once a wide glyph is in front.  `中文 $X$` puts the atom at char index 3 but
    /// at cell 5, and a char-index placement draws the image two cells left of its own source.
    #[test]
    fn a_wide_glyph_before_the_formula_does_not_shift_its_image() {
        reset();
        force_enabled(true);
        force_protocol(Protocol::Sixel);
        let mut st = state("中文 $X$ here.\n");
        let area = Rect::new(0, 0, 40, 3);
        let mut buf = TuiBuf::empty(area);
        paint(&mut st, area, &mut buf);
        let carrier = (0..area.width)
            .find(|x| {
                buf.cell((*x, 0))
                    .is_some_and(|c| c.symbol().contains("q\"1;1;"))
            })
            .expect("the payload is on the row");
        // 中文 is four cells, the space one more, so the atom's first cell is column 5.
        assert_eq!(carrier, 5, "placed by cell column, not by character index");
        reset();
    }

    /// F4: the fallback writes the *source*, so it cannot be clamped to the atom's ink width —
    /// `\frac{1}{2}` is five cells of ink and eleven characters of source.
    #[test]
    fn a_fallback_shows_the_whole_source_not_just_the_ink_width() {
        reset();
        force_enabled(true);
        force_protocol(Protocol::Sixel);
        let mut st = state("one\ntwo\nthree\nholds $\\frac{1}{2}$ here.\n");
        let area = Rect::new(0, 0, 40, 4);
        let mut buf = TuiBuf::empty(area);
        paint(&mut st, area, &mut buf);
        let row: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 3)).map(|c| c.symbol().to_owned()))
            .collect();
        assert!(
            row.contains("\\frac{1}{2}"),
            "the whole source, not its ink-width prefix: {row:?}"
        );
        reset();
    }

    /// F2: a reflowed paragraph that is *revealed* shows its raw source, so the atoms living in
    /// its rendered rows are not on screen at all - painting them puts an image on top of the text
    /// being edited.  The reveal changes how many rows the block occupies, which is why the painter
    /// has to ask `EffectiveRows` (what the view asks) instead of counting `parsed.lines`.
    #[test]
    fn a_revealed_raw_row_never_gets_an_image() {
        reset();
        force_enabled(true);
        force_protocol(Protocol::Sixel);
        // Two source lines in one paragraph: reflow joins them for the rendered form, the raw form
        // keeps them apart - so the two forms differ in row count, which is the whole hazard.
        // The formula must sit on a source line the cursor is *not* on: the inline reveal is
        // per line, so a formula on the cursor's own line is already source and there would be no
        // atom to (wrongly) paint.  The block-level reveal, by contrast, is per block.
        let mut st = state(
            "A paragraph whose first source line is plain,\nand a formula $X$ on its second one.\n",
        );
        st.set_reflow(true);
        st.mode = Mode::Rendered;
        // `is_reflowed_paragraph_at` asks about the *reflowed* form, so the parse has to be redone
        // with reflow on before the reveal can target this paragraph.
        st.refresh_parsed();
        st.update_cursor_block();
        // The reveal is time-gated; the steady state is what the painter sees after the beat.
        st.cursor_block_entered_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1));

        let width = 30usize;
        let area = Rect::new(0, 0, width as u16, 12);
        let mut buf = TuiBuf::empty(area);
        paint(&mut st, area, &mut buf);

        let effective = st.effective_rows(width);
        assert!(
            effective.has_reveal(),
            "the reveal is not in effect (mode/reflow/cursor), so this test would prove nothing"
        );
        let raw_rows: Vec<usize> = (0..area.height as usize)
            .filter(|y| {
                matches!(
                    effective.line_at_visual_row(st.scroll + y),
                    crate::editor::effective_rows::RowHit::Raw { .. }
                )
            })
            .collect();
        let painted: Vec<u16> = (0..area.height)
            .filter(|y| {
                (0..area.width).any(|x| {
                    buf.cell((x, *y))
                        .is_some_and(|c| c.symbol().contains("q\"1;1;"))
                })
            })
            .collect();

        assert!(
            !raw_rows.is_empty(),
            "the reveal must actually be in effect, or this test means nothing"
        );
        assert!(
            painted.is_empty(),
            "no image may sit on a revealed raw row: painted rows {painted:?}, raw rows {raw_rows:?}"
        );
        // And the atom is genuinely there to be painted: this is what makes the assertion above
        // mean something rather than relying on the formula being absent.
        assert!(
            st.parsed
                .lines
                .iter()
                .any(|l| l.spans.iter().any(|sp| sp.content.contains('\u{E000}'))),
            "the rendered lines must still carry an atom span for the guard to be tested"
        );
        reset();
    }

    /// The atoms' reserved widths as the last build left them.
    fn atom_cells() -> Vec<Option<u16>> {
        ATOMS.with(|a| a.borrow().iter().map(|x| x.cells).collect())
    }

    /// The frame the atom moved on must carry it at the *new* cell, and the frame it left must
    /// hand the terminal a delete — a placement is anchored to cells, so nothing else removes it.
    #[test]
    fn scrolling_re_places_the_atom_and_deletes_the_old_placement() {
        reset();
        force_enabled(true);
        let mut st = state("alpha\n\nx $x$ beta\n");
        let area = Rect::new(0, 0, 24, 4);
        let cells = ATOMS
            .with(|a| a.borrow()[0].cells)
            .expect("the atom paints");

        let mut first = TuiBuf::empty(area);
        st.scroll = 0;
        paint(&mut st, area, &mut first);
        let at_rest = carrier(&first).expect("the atom is painted");
        assert!(at_rest.contains("a=p"), "{at_rest:?}");
        assert!(
            at_rest.contains(&format!(",c={cells},r=1,")),
            "the atom's measured cells: {at_rest:?}"
        );
        // Rendered lines: "alpha" / blank / "x $x$ beta", so the atom sits on screen row 3.
        assert!(
            at_rest.contains("\x1b[3;"),
            "placed at the atom's cell: {at_rest:?}"
        );

        // Scroll one row: the atom moves up, so its symbol must name the new cell.
        let mut second = TuiBuf::empty(area);
        st.scroll = 1;
        paint(&mut st, area, &mut second);
        let moved = carrier(&second).expect("the atom is still painted");
        assert_ne!(moved, at_rest, "a moved atom re-places at its new cell");
        assert!(moved.contains("\x1b[2;"), "one row up: {moved:?}");

        // Scrolled off the viewport entirely: a delete, and no placement.
        let mut third = TuiBuf::empty(area);
        st.scroll = 40;
        paint(&mut st, area, &mut third);
        assert!(carrier(&third).is_none(), "nothing to paint off-screen");
        assert!(
            third
                .cell((0, 0))
                .expect("the area's first cell")
                .symbol()
                .contains("a=d"),
            "the abandoned placement must be deleted"
        );
        reset();
    }

    /// The symbol of the first cell whose content carries a kitty escape.
    fn carrier(buf: &TuiBuf) -> Option<String> {
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let symbol = buf.cell((x, y))?.symbol();
                if symbol.contains("a=p") {
                    return Some(symbol.to_owned());
                }
            }
        }
        None
    }

    /// A rendered atom's cells must be blanked of glyphs: the carrier carries the escape and the
    /// rest are `Skip`, so the sentinel cannot be painted through the image.
    #[test]
    fn the_atom_cells_carry_the_escape_and_no_glyphs() {
        let mut canvas = Rect::new(0, 0, 20, 3);
        canvas.x = 0;
        let mut buf = TuiBuf::empty(canvas);
        let geometry = Geometry {
            cells: (3, 1),
            font: (10, 20),
        };
        let dst = Rect {
            x: 0,
            y: 0,
            width: 3,
            height: 1,
        };
        let symbol = placement_symbol(
            7,
            1,
            geometry,
            0,
            dst,
            Some("\x1b_Gq=2,i=7,a=t,m=0;AA\x1b\\"),
        );
        assert!(symbol.contains("a=p"), "{symbol:?}");
        assert!(
            symbol.contains("c=3,r=1"),
            "the placement is the atom's cells: {symbol:?}"
        );
        assert!(
            symbol.contains("w=30,h=20"),
            "the source rect is the cell box: {symbol:?}"
        );
        assert!(
            symbol.contains("C=1"),
            "the terminal must not move the cursor: {symbol:?}"
        );
        assert!(
            symbol.ends_with("\x1b[1;2H"),
            "the symbol must restore the cell cursor: {symbol:?}"
        );
        buf.cell_mut((0u16, 0u16)).unwrap().set_symbol(&symbol);
        assert!(buf
            .cell((0, 0))
            .expect("the carrier cell exists")
            .symbol()
            .contains("a=p"));
    }

    /// A refused placement leaves the source readable instead of the invisible sentinel.
    #[test]
    fn a_refused_atom_falls_back_to_its_source() {
        let area = Rect::new(0, 0, 20, 2);
        let mut buf = TuiBuf::empty(area);
        let dst = Rect {
            x: 0,
            y: 0,
            width: 5,
            height: 1,
        };
        paint_source_fallback(&mut buf, dst, "$Y_1$", Style::default());
        let line: String = (0..5)
            .map(|x| buf.cell((x, 0)).expect("cell").symbol().to_owned())
            .collect();
        assert_eq!(line, "$Y_1$");
    }
}
