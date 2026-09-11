//! Shared chrome for the family of modals built on [`crate::ui::ModalView`].
//!
//! `ModalChrome` owns the [`ModalState`] (scroll, focus, cached button / esc rects), the
//! [`ModalKind`], and the `dismissable` flag, and centralizes the input plumbing.  A concrete
//! modal supplies only its `title` / `body` / `buttons` and its mapping from [`ModalResponse`]
//! to [`super::ModalOutcome`]; both the key and click paths funnel through that one mapping, so
//! mouse and keyboard behave identically and footer buttons are clickable for free.
//!
//! Modals with bespoke key handling intercept the keys they care about and delegate the rest to
//! `on_key`.  A modal needing a pinned control row above the buttons instead builds on the
//! `scroll_container` primitives directly; see [`crate::ui::diff_intro_modal`].

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use super::types::{ModalKind, ModalRenderCtx};
use crate::ui::modal::LinkableResponse;
use crate::ui::modal_links::ModalLink;
use crate::ui::{ModalButton, ModalResponse, ModalState, ModalView};

/// Ceiling on the columns a chrome-backed body can use, given the whole terminal `area`.
///
/// The modal sizes itself to its content, so the real width is unknown until the content
/// exists, but the ceiling is not: [`ModalView`] clamps to the terminal and floors the padding
/// at [`crate::ui::MIN_PAD_H`] per side.  Bodies built for their width use this.
pub fn body_columns(area: Rect) -> u16 {
    area.width.saturating_sub(2 * crate::ui::MIN_PAD_H)
}

/// Shared state + input plumbing for [`ModalView`]-backed modals.
pub struct ModalChrome {
    /// Scroll / focus / cached hit-rects.  `pub` for the rare caller (and the tests) that need
    /// the raw rects.
    pub state: ModalState,
    kind: ModalKind,
    dismissable: bool,
    /// Optional prose width cap forwarded to [`ModalView`] each frame; set once at
    /// construction, like `kind` and `dismissable`.
    max_content_w: Option<u16>,
}

impl ModalChrome {
    /// Build chrome with the given visual urgency and dismissability.  Both values feed
    /// [`ModalView`], the `handle_key` gate, and the `Modal` accessors from this one place, so
    /// they can't drift apart.
    pub fn new(kind: ModalKind, dismissable: bool) -> Self {
        Self {
            state: ModalState::new(),
            kind,
            dismissable,
            max_content_w: None,
        }
    }

    /// Cap the body's content width so prose wraps at a readable measure.  Chain onto `new()`;
    /// see [`crate::ui::ModalView::with_max_content_width`].
    pub fn with_max_content_width(mut self, width: u16) -> Self {
        self.max_content_w = Some(width);
        self
    }

    // ── Accessors ──────────────────────────────────────────────────────────

    pub fn kind(&self) -> ModalKind {
        self.kind
    }

    pub fn dismissable(&self) -> bool {
        self.dismissable
    }

    // ── Render ─────────────────────────────────────────────────────────────

    /// Render the framed modal.  `title` / `body` / `buttons` are supplied each frame so modals
    /// with live content stay in control of their text; the chrome owns only the frame, scroll,
    /// and hit-rects.
    pub fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        ctx: &ModalRenderCtx<'_>,
        title: &str,
        body: &[Line<'_>],
        buttons: &[ModalButton],
    ) {
        self.render_with_links(frame, area, ctx, title, body, buttons, &[]);
    }

    /// [`Self::render`] for a modal carrying inline body links.  `links` is rebuilt each frame
    /// alongside `body`; the modal reads [`Self::focused_link`] to style the focused one.  An
    /// empty slice is exactly `render`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_with_links(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        ctx: &ModalRenderCtx<'_>,
        title: &str,
        body: &[Line<'_>],
        buttons: &[ModalButton],
        links: &[ModalLink],
    ) {
        let mut view = ModalView::new(title, body, buttons, ctx.theme, self.kind, self.dismissable);
        if let Some(w) = self.max_content_w {
            view = view.with_max_content_width(w);
        }
        if !links.is_empty() {
            view = view.with_links(links);
        }
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    // ── Input ──────────────────────────────────────────────────────────────

    /// Translate a key event into a [`ModalResponse`].  Scroll keys are absorbed and reported
    /// as `Continue`.
    pub fn on_key(&mut self, key: &KeyEvent, num_buttons: usize) -> ModalResponse {
        self.state.handle_key(key, num_buttons, self.dismissable)
    }

    /// [`Self::on_key`] for a link-bearing modal: Tab walks one ring over links then buttons,
    /// and Enter on a link reports [`LinkableResponse::Link`].
    pub(crate) fn on_key_linkable(
        &mut self,
        key: &KeyEvent,
        num_links: usize,
        num_buttons: usize,
    ) -> LinkableResponse {
        self.state
            .handle_key_linkable(key, num_links, num_buttons, self.dismissable)
    }

    /// [`Self::on_click`] for a link-bearing modal: a click inside a link's rect reports
    /// [`LinkableResponse::Link`].
    pub(crate) fn on_click_linkable(&self, col: u16, row: u16) -> LinkableResponse {
        self.state.handle_click_linkable(col, row, self.dismissable)
    }

    /// Which body link holds focus, for the render pass to style.
    pub(crate) fn focused_link(&self) -> Option<usize> {
        self.state.focused_link
    }

    /// Translate a left-click into a [`ModalResponse`] — a footer button, the `esc` affordance,
    /// or `Continue`.
    pub fn on_click(&self, col: u16, row: u16) -> ModalResponse {
        self.state.handle_click(col, row, self.dismissable)
    }

    /// Scroll the body by a mouse-wheel `delta`.
    pub fn on_wheel(&mut self, delta: i32) {
        self.state.scroll_by(delta);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::PROSE_CONTENT_WIDTH;

    #[test]
    fn the_width_cap_is_opt_in() {
        // This is the only production `ModalView::new` call site, so the default must stay
        // `None` (size-to-content) for every modal that never asks for a cap.
        assert_eq!(
            ModalChrome::new(ModalKind::Normal, true).max_content_w,
            None
        );
    }

    #[test]
    fn the_width_cap_builder_is_chainable_onto_new() {
        let chrome =
            ModalChrome::new(ModalKind::Warning, false).with_max_content_width(PROSE_CONTENT_WIDTH);
        assert_eq!(chrome.max_content_w, Some(PROSE_CONTENT_WIDTH));
        assert_eq!(chrome.kind(), ModalKind::Warning);
        assert!(!chrome.dismissable());
    }
}
