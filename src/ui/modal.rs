//! A reusable modal popup: a centered box with a title, body lines, and a footer button row.
//! Left/Right or Tab/Shift-Tab cycle focus, Enter activates, Escape cancels when dismissable.
//! UI-only: it returns a `ModalResponse` and the caller handles the consequences.
//!
//! Bodies that overflow scroll (keys, plus mouse wheel via [`ModalState::scroll_by`]) with a
//! [`crate::ui::scrollbar`] beside them. Scroll arithmetic, frame rendering, and sizing live
//! in [`crate::ui::scroll_container`], shared with the overlays.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, StatefulWidget, Widget, Wrap},
};

use crate::config::Theme;
use crate::ui::button_row::{button_rows_height, buttons_row_width, render_buttons, Button};
use crate::ui::modal_links::{link_rects, wrap_rows, ModalLink, WrappedRow};
use crate::ui::scroll_container::{
    centered_rect_for_content, compute_pad_h, draw_frame, wrapped_rows, ContentSize, FrameOpts,
    ModalKind, ScrollContainerState, MAX_PAD_H, VERTICAL_CHROME_ROWS,
};
use crate::ui::scrollbar;

/// True when `(col, row)` falls inside `r`.
fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

/// [`rect_contains`] over an optional rect — `None` never matches.
fn rect_contains_opt(r: Option<Rect>, col: u16, row: u16) -> bool {
    r.is_some_and(|r| rect_contains(r, col, row))
}

/// The outcome of a key event handed to the modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalResponse {
    /// The modal remains open and just re-renders.
    Continue,
    /// The user activated the button at this index.
    ButtonPressed(usize),
    /// The user dismissed with Escape without activating a specific button.
    Cancelled,
}

/// [`ModalResponse`] widened with the outcome only a link-bearing modal can produce.
///
/// A parallel type rather than a new variant: every `resolve()` in `app::modal` matches
/// `ModalResponse` exhaustively, so a fourth variant would touch twenty-two files that never
/// carry a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkableResponse {
    /// Everything a button-only modal produces.
    Modal(ModalResponse),
    /// The user activated the body link at this index into the modal's `[ModalLink]` slice.
    Link(usize),
}

/// A modal button (label only); rendered wrapped in `[ … ]` via [`Button`].
#[derive(Debug, Clone)]
pub struct ModalButton {
    pub label: String,
}

impl ModalButton {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

/// Mutable modal state: focus, resolution, scroll bookkeeping, and last-render hit rects.
#[derive(Debug, Clone, Default)]
pub struct ModalState {
    pub focused: usize,
    /// Set by `handle_key` once the user activates a button or cancels.
    pub response: Option<ModalResponse>,
    pub scroll_state: ScrollContainerState,
    /// Rect of the rendered `esc` hint, for
    /// [`crate::app::modal::types::close_if_esc_clicked`].
    pub esc_button_rect: Option<Rect>,
    /// Rect of each footer button, in button order; empty when there are none.
    pub button_rects: Vec<Rect>,
    /// `Some(i)` when body link `i` holds focus. `focused` always stays the focused *button*
    /// index, so button-only modals read it exactly as before.
    pub(crate) focused_link: Option<usize>,
    /// `(link index, rect)` per visible link *row* (a link wrapped across two rows appears
    /// twice, as in [`crate::ui::link_view`]). Empty for a modal without links.
    pub(crate) link_rects: Vec<(usize, Rect)>,
}

impl ModalState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adjust scroll by `delta` rows, clamped. The mouse-wheel path; keys go through
    /// `scroll_state.handle_scroll_key`.
    pub fn scroll_by(&mut self, delta: i32) {
        self.scroll_state.scroll_by(delta);
    }

    /// Footer button whose last-rendered rect contains `(col, row)`.
    pub fn button_at(&self, col: u16, row: u16) -> Option<usize> {
        self.button_rects
            .iter()
            .position(|r| rect_contains(*r, col, row))
    }

    /// Body link whose last-rendered rect contains `(col, row)`; scrolled-away links have no
    /// rect and cannot be hit.
    pub(crate) fn link_at(&self, col: u16, row: u16) -> Option<usize> {
        self.link_rects
            .iter()
            .find(|(_, r)| rect_contains(*r, col, row))
            .map(|(i, _)| *i)
    }

    /// [`Self::handle_click`] widened with body links, which take priority (a link is inside
    /// the body, so nothing else can overlap it).
    pub(crate) fn handle_click_linkable(
        &self,
        col: u16,
        row: u16,
        dismissable: bool,
    ) -> LinkableResponse {
        match self.link_at(col, row) {
            Some(i) => LinkableResponse::Link(i),
            None => LinkableResponse::Modal(self.handle_click(col, row, dismissable)),
        }
    }

    /// Translate a left-click into the same [`ModalResponse`] the keyboard path produces:
    /// footer button, else the `esc` hint (when `dismissable`), else `Continue`. The single
    /// definition of footer click hit-testing for every `ModalView`-backed modal.
    pub fn handle_click(&self, col: u16, row: u16, dismissable: bool) -> ModalResponse {
        if let Some(idx) = self.button_at(col, row) {
            return ModalResponse::ButtonPressed(idx);
        }
        if dismissable && rect_contains_opt(self.esc_button_rect, col, row) {
            return ModalResponse::Cancelled;
        }
        ModalResponse::Continue
    }

    /// Handle a key event. The response is also cached on `self.response`. `dismissable`
    /// gates `Esc` and `n`/`N`.
    pub fn handle_key(
        &mut self,
        key: &crossterm::event::KeyEvent,
        num_buttons: usize,
        dismissable: bool,
    ) -> ModalResponse {
        match self.handle_key_linkable(key, 0, num_buttons, dismissable) {
            LinkableResponse::Modal(r) => r,
            // Unreachable with `num_links == 0`.
            LinkableResponse::Link(_) => ModalResponse::Continue,
        }
    }

    /// The real key handler. Tab / Left / Right walk **one ring over links then buttons**, so
    /// a body link is reachable without a mouse (mouse support is capability-gated). With
    /// `num_links == 0` every branch collapses to the button-only behavior.
    pub(crate) fn handle_key_linkable(
        &mut self,
        key: &crossterm::event::KeyEvent,
        num_links: usize,
        num_buttons: usize,
        dismissable: bool,
    ) -> LinkableResponse {
        use crossterm::event::{KeyCode, KeyModifiers};
        // Scroll keys never dismiss the modal; they run even for button-less modals.
        if self.scroll_state.handle_scroll_key(key) {
            return LinkableResponse::Modal(ModalResponse::Continue);
        }
        let has_buttons = num_buttons > 0;
        let ring_len = num_links + num_buttons;
        // Ring positions `0..num_links` are links; the rest are buttons. `None` means no
        // focus at all — the resting state of a modal with links and no buttons (the
        // config-warning modal). Seeding a phantom button there made Enter report
        // `ButtonPressed(0)` against an empty list and the first Tab land on link 1.
        let pos: Option<usize> = match self.focused_link {
            Some(i) => Some(i),
            None => has_buttons.then_some(num_links + self.focused),
        };
        let response = match key.code {
            KeyCode::Left | KeyCode::BackTab if ring_len > 0 => {
                // Unfocused, stepping backwards enters at the far end.
                let next = pos.map_or(ring_len - 1, |p| (p + ring_len - 1) % ring_len);
                self.set_ring_pos(next, num_links);
                ModalResponse::Continue
            }
            KeyCode::Right | KeyCode::Tab if ring_len > 0 => {
                let next = pos.map_or(0, |p| (p + 1) % ring_len);
                self.set_ring_pos(next, num_links);
                ModalResponse::Continue
            }
            // A focused link swallows Enter; with no buttons there is nothing to press.
            KeyCode::Enter | KeyCode::Char(' ') if ring_len > 0 => {
                if let Some(i) = self.focused_link {
                    return LinkableResponse::Link(i);
                }
                if !has_buttons {
                    return LinkableResponse::Modal(ModalResponse::Continue);
                }
                ModalResponse::ButtonPressed(self.focused)
            }
            KeyCode::Esc if dismissable => ModalResponse::Cancelled,
            // No-modifier `n`/`y` shortcuts, so embedded text editors don't lose letters.
            KeyCode::Char('n') | KeyCode::Char('N')
                if dismissable && key.modifiers == KeyModifiers::NONE =>
            {
                ModalResponse::Cancelled
            }
            // `y` presses the focused *button* only; with a link focused it would fire
            // whatever button the ring last sat on.
            KeyCode::Char('y') | KeyCode::Char('Y')
                if has_buttons
                    && self.focused_link.is_none()
                    && key.modifiers == KeyModifiers::NONE =>
            {
                ModalResponse::ButtonPressed(self.focused)
            }
            _ => ModalResponse::Continue,
        };
        if !matches!(response, ModalResponse::Continue) {
            self.response = Some(response.clone());
        }
        LinkableResponse::Modal(response)
    }

    /// Park the focus ring on `pos`. `focused` is left alone while a link holds focus, so
    /// Tabbing back onto the buttons returns to the button the user started from.
    fn set_ring_pos(&mut self, pos: usize, num_links: usize) {
        if pos < num_links {
            self.focused_link = Some(pos);
        } else {
            self.focused_link = None;
            self.focused = pos - num_links;
        }
    }
}

/// The modal widget; renders on top of whatever the underlying view drew. `body` is styled
/// `Line`s (the cheat sheet mirrors preview styling on top of raw syntax).
pub struct ModalView<'a> {
    pub title: &'a str,
    pub body: &'a [Line<'a>],
    pub buttons: &'a [ModalButton],
    pub theme: &'a Theme,
    /// Visual urgency — drives title color.
    pub kind: ModalKind,
    /// When false, no `esc` hint is rendered and `Esc` is ignored — for modals that gate the
    /// user on an explicit choice.
    pub dismissable: bool,
    /// Maximum horizontal padding per side; [`MAX_PAD_H`] via [`ModalView::new`].
    pub max_pad_h: u16,
    /// Cap on the body's content width before padding. `None` sizes to the longest body line
    /// — right for tables, wrong for prose, whose unwrapped width fills the terminal. Never
    /// narrows the modal below its button row (see the clamp in `render`).
    pub max_content_w: Option<u16>,
    /// Inline body links by coordinate — see [`crate::ui::modal_links`]. Non-empty switches
    /// `render` onto the pre-wrapped path, the only way to know where a span landed.
    pub(crate) links: &'a [ModalLink],
}

impl<'a> ModalView<'a> {
    /// Construct with the default padding. Production callers go through this so a default
    /// change picks them up.
    pub fn new(
        title: &'a str,
        body: &'a [Line<'a>],
        buttons: &'a [ModalButton],
        theme: &'a Theme,
        kind: ModalKind,
        dismissable: bool,
    ) -> Self {
        Self {
            title,
            body,
            buttons,
            theme,
            kind,
            dismissable,
            max_pad_h: MAX_PAD_H,
            max_content_w: None,
            links: &[],
        }
    }

    /// Override the maximum horizontal padding.
    #[allow(dead_code)]
    pub fn with_max_pad_h(mut self, max_pad_h: u16) -> Self {
        self.max_pad_h = max_pad_h;
        self
    }

    /// Cap the body's content width so prose wraps at a readable measure.
    pub fn with_max_content_width(mut self, width: u16) -> Self {
        self.max_content_w = Some(width);
        self
    }

    /// Declare inline body links. A link naming a span that does not exist never produces a
    /// rect, so a stale coordinate degrades to an unclickable label rather than a panic.
    pub(crate) fn with_links(mut self, links: &'a [ModalLink]) -> Self {
        self.links = links;
        self
    }
}

impl<'a> StatefulWidget for ModalView<'a> {
    type State = ModalState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let body_width = self.body.iter().map(|l| l.width()).max().unwrap_or(0) as u16;
        let button_specs: Vec<Button> = self
            .buttons
            .iter()
            .map(|b| Button::bracketed(b.label.as_str()))
            .collect();
        let button_width = buttons_row_width(&button_specs);
        // A capped modal still has to fit its button row; everything downstream reads this
        // one value so the sizing pass and the render agree.
        let content_width = match self.max_content_w {
            Some(cap) => body_width.min(cap.max(button_width)).max(button_width),
            None => body_width.max(button_width),
        };
        // Derive the wrap width using the same padding rule `draw_frame` applies
        // (`compute_pad_h`); otherwise a line that wraps at the clamped width leaves the
        // modal too short, and a narrow terminal floors the padding at MIN_PAD_H while this
        // pass subtracts the full MAX.
        let prospective_modal_width = content_width
            .saturating_add(2 * self.max_pad_h)
            .min(area.width);
        let prospective_pad_h =
            compute_pad_h(prospective_modal_width, content_width, self.max_pad_h);
        let prospective_body_inner_w = prospective_modal_width
            .saturating_sub(2 * prospective_pad_h)
            .max(1);
        // The body renders as a block of its own width centered in the modal, so measure it
        // at that width.
        let prospective_body_render_w = body_width.clamp(1, prospective_body_inner_w);
        let wrapped_body_height = wrapped_rows(self.body, prospective_body_render_w);
        // A footer too narrow for every button wraps rather than clips, so its height is a
        // function of the width — asked at the same width `render_buttons` packs against.
        let button_row_count = if self.buttons.is_empty() {
            0
        } else {
            button_rows_height(&button_specs, prospective_body_inner_w).max(1)
        };
        let pinned_bottom: u16 = if self.buttons.is_empty() {
            0
        } else {
            1 + button_row_count
        };
        let content = ContentSize {
            width: content_width,
            height: wrapped_body_height,
            pinned_top: 0,
            pinned_bottom,
            max_pad_h: self.max_pad_h,
        };
        let modal_area = centered_rect_for_content(content, area);

        let body_inner_h = modal_area.height.saturating_sub(VERTICAL_CHROME_ROWS);
        let text_body_height = body_inner_h.saturating_sub(pinned_bottom);
        // The scrollbar paints into the rightmost padding column, not inside the body, so the
        // wrap width is the full inner width.
        let pad_h = compute_pad_h(modal_area.width, content_width, self.max_pad_h);
        let body_inner_w = modal_area.width.saturating_sub(2 * pad_h).max(1);
        let body_render_w = body_width.clamp(1, body_inner_w);
        // A link-bearing body is wrapped here rather than by `Paragraph` (see `modal_links`);
        // `wrap_rows` reproduces `Wrap { trim: false }` row for row, so the scroll math is
        // shared.
        let wrapped: Option<Vec<WrappedRow>> =
            (!self.links.is_empty()).then(|| wrap_rows(self.body, body_render_w));
        let total = match &wrapped {
            Some(rows) => rows.len() as u16,
            None => wrapped_rows(self.body, body_render_w),
        };
        state.scroll_state.observe(total, text_body_height);

        let layout = draw_frame(
            modal_area,
            buf,
            FrameOpts {
                title: self.title,
                kind: self.kind,
                show_close_hint: self.dismissable,
                content,
                theme: self.theme,
            },
        );
        state.esc_button_rect = layout.esc_hit_rect;
        let inner = layout.body;
        if inner.height == 0 || inner.width == 0 {
            // Nothing painted, nothing clickable: stale rects would let a click follow a link
            // that is not on screen.
            state.link_rects.clear();
            return;
        }

        // Pre-wrapped rows get no `Wrap`: ratatui then truncates, so a residual disagreement
        // with `WordWrapper` clips one trailing cell instead of reflowing the row out from
        // under its link rects.
        let body_paragraph = match &wrapped {
            Some(rows) => Paragraph::new(rows.iter().map(|r| r.line.clone()).collect::<Vec<_>>())
                .style(self.theme.modal_bg),
            None => Paragraph::new(self.body.to_vec())
                .wrap(Wrap { trim: false })
                .style(self.theme.modal_bg),
        };

        // Center the body as a block when the footer is wider than it; left-aligning pushes a
        // self-centered body (the About page) off-center by half the difference.
        let body_area = Rect {
            x: inner.x + inner.width.saturating_sub(body_render_w) / 2,
            y: inner.y,
            width: body_render_w,
            height: text_body_height,
        };
        body_paragraph
            .scroll((state.scroll_state.scroll, 0))
            .render(body_area, buf);

        // After `body_area` is final, so the rects are absolute terminal coordinates.
        state.link_rects = match &wrapped {
            Some(rows) => link_rects(rows, self.links, body_area, state.scroll_state.scroll),
            None => Vec::new(),
        };

        // Scrollbars are visual-only; drag would need mouse events routed into `Modal`.
        if state.scroll_state.max_scroll() > 0 {
            let bar_area = Rect {
                x: layout.scrollbar_col,
                y: body_area.y,
                width: 1,
                height: body_area.height,
            };
            scrollbar::render_for_scroll_state(bar_area, &state.scroll_state, self.theme, buf);
        }

        if !self.buttons.is_empty() {
            let button_area = Rect {
                x: inner.x,
                y: inner.y + inner.height.saturating_sub(button_row_count),
                width: inner.width,
                height: button_row_count,
            };
            state.button_rects =
                render_buttons(button_area, buf, &button_specs, state.focused, self.theme);
        } else {
            state.button_rects.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    use crate::ui::scroll_container::PROSE_CONTENT_WIDTH;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // ── The focus ring on a modal with links and no buttons ───────

    /// The config-warning modal's shape: one link, no footer buttons.
    #[test]
    fn enter_presses_nothing_on_a_button_less_modal() {
        let mut st = ModalState::default();
        assert_eq!(
            st.handle_key_linkable(&key(KeyCode::Enter), 1, 0, true),
            LinkableResponse::Modal(ModalResponse::Continue),
            "there is no button 0 to press"
        );
        st.handle_key_linkable(&key(KeyCode::Tab), 1, 0, true);
        assert_eq!(
            st.handle_key_linkable(&key(KeyCode::Enter), 1, 0, true),
            LinkableResponse::Link(0)
        );
    }

    /// With no button to start from, the first Tab enters at the first link.
    #[test]
    fn the_first_tab_reaches_the_first_link_when_there_are_no_buttons() {
        let mut st = ModalState::default();
        st.handle_key_linkable(&key(KeyCode::Tab), 3, 0, true);
        assert_eq!(st.focused_link, Some(0));
        st.handle_key_linkable(&key(KeyCode::Tab), 3, 0, true);
        assert_eq!(st.focused_link, Some(1));
    }

    /// Backwards enters at the far end.
    #[test]
    fn the_first_back_tab_reaches_the_last_link_when_there_are_no_buttons() {
        let mut st = ModalState::default();
        st.handle_key_linkable(&key(KeyCode::BackTab), 3, 0, true);
        assert_eq!(st.focused_link, Some(2));
    }

    /// A modal with buttons rests on button 0; the first Tab steps off it.
    #[test]
    fn a_modal_with_buttons_still_rests_on_its_first_button() {
        let mut st = ModalState::default();
        assert_eq!(st.focused_link, None);
        assert_eq!(
            st.handle_key_linkable(&key(KeyCode::Enter), 1, 2, true),
            LinkableResponse::Modal(ModalResponse::ButtonPressed(0))
        );
        st.handle_key_linkable(&key(KeyCode::Tab), 1, 2, true);
        assert_eq!(st.focused, 1, "Tab walks to the second button first");
        st.handle_key_linkable(&key(KeyCode::Tab), 1, 2, true);
        assert_eq!(st.focused_link, Some(0), "then wraps onto the link");
    }

    fn state_with_scroll(scroll: u16, total: u16, visible: u16) -> ModalState {
        ModalState {
            scroll_state: ScrollContainerState {
                scroll,
                last_total: total,
                last_visible: visible,
            },
            ..ModalState::new()
        }
    }

    #[test]
    fn tab_cycles_focus_forward() {
        let mut state = ModalState::new();
        assert_eq!(state.focused, 0);
        state.handle_key(&key(KeyCode::Tab), 3, true);
        assert_eq!(state.focused, 1);
        state.handle_key(&key(KeyCode::Tab), 3, true);
        assert_eq!(state.focused, 2);
        state.handle_key(&key(KeyCode::Tab), 3, true);
        assert_eq!(state.focused, 0); // wraps
    }

    #[test]
    fn left_cycles_focus_backward_with_wrap() {
        let mut state = ModalState::new();
        state.handle_key(&key(KeyCode::Left), 3, true);
        assert_eq!(state.focused, 2);
        state.handle_key(&key(KeyCode::Left), 3, true);
        assert_eq!(state.focused, 1);
    }

    #[test]
    fn enter_activates_focused_button() {
        let mut state = ModalState::new();
        state.focused = 1;
        let response = state.handle_key(&key(KeyCode::Enter), 2, true);
        assert_eq!(response, ModalResponse::ButtonPressed(1));
        assert_eq!(state.response, Some(ModalResponse::ButtonPressed(1)));
    }

    #[test]
    fn escape_cancels() {
        let mut state = ModalState::new();
        let response = state.handle_key(&key(KeyCode::Esc), 2, true);
        assert_eq!(response, ModalResponse::Cancelled);
    }

    #[test]
    fn escape_does_not_dismiss_when_not_dismissable() {
        let mut state = ModalState::new();
        let response = state.handle_key(&key(KeyCode::Esc), 2, false);
        assert_eq!(response, ModalResponse::Continue);
    }

    #[test]
    fn render_draws_title_body_and_buttons() {
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body = vec![Line::raw("Hello."), Line::raw("World.")];
        let buttons = vec![ModalButton::new("Ok"), ModalButton::new("Cancel")];
        terminal
            .draw(|frame| {
                let m = ModalView::new("Notice", &body, &buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();

        let contents: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(contents.contains("Notice"), "title missing: {contents}");
        assert!(contents.contains("Hello."), "body missing: {contents}");
        assert!(contents.contains("Ok"), "button missing: {contents}");
        assert!(contents.contains("Cancel"), "button missing: {contents}");
        assert!(contents.contains("esc"), "esc hint missing: {contents}");
        assert!(state.esc_button_rect.is_some());
    }

    #[test]
    fn render_omits_esc_hint_when_not_dismissable() {
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body = vec![Line::raw("Choose:")];
        let buttons = vec![ModalButton::new("Ok"), ModalButton::new("No")];
        terminal
            .draw(|frame| {
                let m = ModalView::new(
                    "Pick one",
                    &body,
                    &buttons,
                    theme(),
                    ModalKind::Warning,
                    false,
                );
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        assert!(state.esc_button_rect.is_none());
    }

    #[test]
    fn esc_rect_is_set_after_dismissable_render() {
        // The hit-test itself is exercised by `close_if_esc_clicked`.
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body = vec![Line::raw("hi")];
        let buttons = vec![ModalButton::new("Ok")];
        terminal
            .draw(|frame| {
                let m = ModalView::new("T", &body, &buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        let r = state.esc_button_rect.expect("esc rect populated");
        assert!(r.width > 0 && r.height == 1);
    }

    // ── Scroll behaviour ─────────────────────────────────────────────────

    #[test]
    fn scroll_by_clamps_at_top() {
        let mut state = state_with_scroll(2, 10, 5);
        state.scroll_by(-100);
        assert_eq!(state.scroll_state.scroll, 0);
    }

    #[test]
    fn scroll_by_clamps_at_bottom() {
        let mut state = state_with_scroll(0, 10, 5);
        state.scroll_by(100);
        assert_eq!(state.scroll_state.scroll, 5); // 10 - 5
    }

    #[test]
    fn scroll_by_is_a_noop_when_body_fits() {
        let mut state = state_with_scroll(0, 4, 10);
        state.scroll_by(3);
        assert_eq!(state.scroll_state.scroll, 0);
    }

    #[test]
    fn down_key_advances_scroll_one_line() {
        let mut state = state_with_scroll(0, 20, 5);
        let resp = state.handle_key(&key(KeyCode::Down), 1, true);
        assert_eq!(resp, ModalResponse::Continue);
        assert_eq!(state.scroll_state.scroll, 1);
    }

    #[test]
    fn page_down_jumps_by_visible_height() {
        let mut state = state_with_scroll(0, 30, 10);
        state.handle_key(&key(KeyCode::PageDown), 1, true);
        assert_eq!(state.scroll_state.scroll, 10);
        state.handle_key(&key(KeyCode::PageDown), 1, true);
        assert_eq!(state.scroll_state.scroll, 20);
        state.handle_key(&key(KeyCode::PageDown), 1, true);
        assert_eq!(state.scroll_state.scroll, 20);
    }

    #[test]
    fn home_and_end_jump_to_extremes() {
        let mut state = state_with_scroll(4, 12, 4);
        state.handle_key(&key(KeyCode::End), 1, true);
        assert_eq!(state.scroll_state.scroll, 8); // 12 - 4
        state.handle_key(&key(KeyCode::Home), 1, true);
        assert_eq!(state.scroll_state.scroll, 0);
    }

    #[test]
    fn render_clamps_scroll_when_body_shrinks() {
        // 4 chrome rows + 2 pinned-bottom + 5 body rows = 11 modal rows.
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState {
            scroll_state: ScrollContainerState {
                scroll: 100, // intentionally past the end
                ..ScrollContainerState::default()
            },
            ..ModalState::new()
        };
        let body: Vec<Line<'_>> = (0..5).map(|i| Line::raw(format!("line {i}"))).collect();
        let buttons = vec![ModalButton::new("Ok")];
        terminal
            .draw(|frame| {
                let m = ModalView::new("Notice", &body, &buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        assert_eq!(state.scroll_state.scroll, 0);
    }

    #[test]
    fn render_paints_scrollbar_when_body_overflows() {
        // 60×8: 4 chrome + 2 pinned rows leave 2 body rows visible of 8.
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body: Vec<Line<'_>> = (0..8).map(|i| Line::raw(format!("body {i}"))).collect();
        let buttons = vec![ModalButton::new("Ok")];
        terminal
            .draw(|frame| {
                let m = ModalView::new("Tall", &body, &buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();

        let contents: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            contents.contains('█'),
            "expected scrollbar thumb glyph, got: {contents}"
        );
        assert!(state.scroll_state.last_total > state.scroll_state.last_visible);
    }

    /// Render at `w`x`h` and return the state plus the painted rows.
    fn render_modal(
        w: u16,
        h: u16,
        body: &[Line<'static>],
        buttons: &[ModalButton],
    ) -> (ModalState, Vec<String>) {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut state = ModalState::new();
        terminal
            .draw(|frame| {
                let m = ModalView::new("Title", body, buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let rows = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_owned()).collect())
            .collect();
        (state, rows)
    }

    #[test]
    fn a_footer_too_wide_for_the_modal_wraps_onto_a_second_row() {
        // Clipping would leave a focusable, clickable button off the frame.
        let body = vec![Line::raw("Short.")];
        let buttons = vec![
            ModalButton::new("Release notes"),
            ModalButton::new("Check for updates"),
            ModalButton::new("View on GitHub"),
        ];
        let (state, rows) = render_modal(44, 20, &body, &buttons);
        assert_eq!(state.button_rects.len(), 3);
        let ys: Vec<u16> = state.button_rects.iter().map(|r| r.y).collect();
        assert!(ys[0] < ys[2], "the row wrapped: {ys:?}");
        let painted = rows.join("\n");
        for label in ["Release notes", "Check for updates", "View on GitHub"] {
            assert!(painted.contains(label), "{label} missing:\n{painted}");
        }
        assert!(state
            .button_rects
            .iter()
            .all(|r| r.x + r.width <= 44 && r.y < 20));
    }

    #[test]
    fn a_wrapped_footer_gets_the_rows_it_needs() {
        // The frame grows by a row, which only happens if the sizing pass packs the footer at
        // the width the render does.
        let body: Vec<Line<'static>> = (0..3).map(|i| Line::raw(format!("Body {i}"))).collect();
        let buttons = vec![
            ModalButton::new("Release notes"),
            ModalButton::new("Check for updates"),
            ModalButton::new("View on GitHub"),
        ];
        let (_, rows) = render_modal(44, 20, &body, &buttons);
        let painted = rows.join("\n");
        for i in 0..3 {
            assert!(painted.contains(&format!("Body {i}")), "\n{painted}");
        }
    }

    #[test]
    fn a_body_narrower_than_the_footer_is_centred_in_the_modal() {
        let body = vec![Line::raw("|....|")];
        let buttons = vec![
            ModalButton::new("Check for updates"),
            ModalButton::new("View on GitHub"),
        ];
        let (_, rows) = render_modal(80, 12, &body, &buttons);
        let body_row = rows
            .iter()
            .find(|r| r.contains("|....|"))
            .expect("body row");
        let start = body_row.find('|').unwrap();
        let end = body_row.rfind('|').unwrap() + 1;
        let button_row = rows.iter().find(|r| r.contains("[ View")).expect("footer");
        let b_start = button_row.find('[').unwrap();
        let b_end = button_row.rfind(']').unwrap() + 1;
        // Within the rounding of an odd leftover column.
        let body_centre = start + end;
        let footer_centre = b_start + b_end;
        assert!(
            body_centre.abs_diff(footer_centre) <= 1,
            "body {start}..{end} vs footer {b_start}..{b_end}"
        );
    }

    /// Widest run of cells carrying the modal background.
    fn painted_modal_width(terminal: &Terminal<TestBackend>) -> u16 {
        let buf = terminal.backend().buffer();
        let area = *buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .filter(|&x| buf[(x, y)].bg == theme().modal_bg.bg.unwrap())
                    .count() as u16
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn uncapped_prose_stretches_to_the_terminal_width() {
        // Baseline for `max_content_width_caps_a_prose_modal`.
        let backend = TestBackend::new(160, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body = vec![Line::raw("word ".repeat(40))];
        let buttons: Vec<ModalButton> = vec![];
        terminal
            .draw(|frame| {
                let m = ModalView::new("Prose", &body, &buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        assert_eq!(painted_modal_width(&terminal), 160);
    }

    #[test]
    fn max_content_width_caps_a_prose_modal() {
        // Capped, the outer width is cap + 2 * MAX_PAD_H.
        let backend = TestBackend::new(160, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body = vec![Line::raw("word ".repeat(40))];
        let buttons: Vec<ModalButton> = vec![];
        terminal
            .draw(|frame| {
                let m = ModalView::new("Prose", &body, &buttons, theme(), ModalKind::Normal, true)
                    .with_max_content_width(80);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        assert_eq!(painted_modal_width(&terminal), 80 + 2 * MAX_PAD_H);
    }

    #[test]
    fn max_content_width_never_clips_the_button_row() {
        // A cap narrower than the buttons loses to them.
        let backend = TestBackend::new(160, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body = vec![Line::raw("word ".repeat(40))];
        let buttons = vec![
            ModalButton::new("A rather long button label"),
            ModalButton::new("And another one"),
        ];
        let button_w = buttons_row_width(
            &buttons
                .iter()
                .map(|b| Button::bracketed(b.label.as_str()))
                .collect::<Vec<_>>(),
        );
        terminal
            .draw(|frame| {
                let m = ModalView::new("Prose", &body, &buttons, theme(), ModalKind::Normal, true)
                    .with_max_content_width(10);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        assert_eq!(painted_modal_width(&terminal), button_w + 2 * MAX_PAD_H);
    }

    #[test]
    fn modal_height_grows_to_fit_wrapped_body_lines() {
        // The modal must size itself for the wrapped row count, not the line count.
        let backend = TestBackend::new(40, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = ModalState::new();
        let body = vec![
            Line::raw(
                "This is a fairly long body line that will definitely wrap inside a 40 column modal.",
            ),
            Line::raw("short tail"),
        ];
        let buttons = vec![ModalButton::new("Ok")];
        terminal
            .draw(|frame| {
                let m = ModalView::new("Wrap", &body, &buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), &mut state);
            })
            .unwrap();
        assert_eq!(state.scroll_state.max_scroll(), 0);
        assert!(
            state.scroll_state.last_visible >= state.scroll_state.last_total,
            "expected body to fit; last_total={}, last_visible={}",
            state.scroll_state.last_total,
            state.scroll_state.last_visible,
        );
    }

    // ── Click hit-testing ────────────────────────────────────────────────

    fn render_two_button_modal(state: &mut ModalState) {
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let body = vec![Line::raw("Pick one.")];
        let buttons = vec![ModalButton::new("Ok"), ModalButton::new("Cancel")];
        terminal
            .draw(|frame| {
                let m = ModalView::new("Notice", &body, &buttons, theme(), ModalKind::Normal, true);
                frame.render_stateful_widget(m, frame.area(), state);
            })
            .unwrap();
    }

    #[test]
    fn button_at_maps_click_to_footer_button_index() {
        let mut state = ModalState::new();
        render_two_button_modal(&mut state);
        assert_eq!(state.button_rects.len(), 2);
        for (idx, rect) in state.button_rects.clone().iter().enumerate() {
            let col = rect.x + rect.width / 2;
            assert_eq!(state.button_at(col, rect.y), Some(idx));
        }
        assert_eq!(state.button_at(0, 0), None);
    }

    #[test]
    fn handle_click_prefers_button_then_esc_then_continue() {
        let mut state = ModalState::new();
        render_two_button_modal(&mut state);
        let btn = state.button_rects[1];
        assert_eq!(
            state.handle_click(btn.x + btn.width / 2, btn.y, true),
            ModalResponse::ButtonPressed(1),
        );
        let esc = state.esc_button_rect.expect("esc rect populated");
        assert_eq!(
            state.handle_click(esc.x, esc.y, true),
            ModalResponse::Cancelled,
        );
        assert_eq!(
            state.handle_click(esc.x, esc.y, false),
            ModalResponse::Continue
        );
        assert_eq!(state.handle_click(0, 0, true), ModalResponse::Continue);
    }

    #[test]
    fn new_uses_default_max_pad_and_builder_overrides_it() {
        let body: [Line<'_>; 0] = [];
        let buttons: [ModalButton; 0] = [];
        let m = ModalView::new("T", &body, &buttons, theme(), ModalKind::Normal, true);
        assert_eq!(m.max_pad_h, MAX_PAD_H);
        let m = ModalView::new("T", &body, &buttons, theme(), ModalKind::Normal, true)
            .with_max_pad_h(8);
        assert_eq!(m.max_pad_h, 8);
    }

    #[test]
    fn width_cap_is_opt_in_and_defaults_off() {
        // Every modal that doesn't ask for a cap must lay out as before the field existed.
        let body: [Line<'_>; 0] = [];
        let buttons: [ModalButton; 0] = [];
        let m = ModalView::new("T", &body, &buttons, theme(), ModalKind::Normal, true);
        assert_eq!(m.max_content_w, None);
        let m = ModalView::new("T", &body, &buttons, theme(), ModalKind::Normal, true)
            .with_max_content_width(PROSE_CONTENT_WIDTH);
        assert_eq!(m.max_content_w, Some(PROSE_CONTENT_WIDTH));
    }
}
