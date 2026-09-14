//! The once-a-day tip notice: a short pointer to a less-obvious feature, chosen by
//! [`super::super::tips`] and orchestrated by [`super::super::tip_notice`].
//!
//! Built on [`ModalChrome`] like [`super::TerminalCapabilitiesModal`]: prose, an optional "learn
//! more" footnote into the manual, and a footer button row.  The "off switch" is a **button**, not
//! an in-body toggle — the chrome family renders prose and buttons only.  It appears only on the
//! daily startup tip ([`DailyTipModal::new`]): that tip interrupts the reader, so it offers "Got
//! it" or turn it off for good with "Don't show tips".  A tip pulled up on demand from the
//! Browse-tips index ([`DailyTipModal::browsing`]) was sought out, not thrust in front of anyone,
//! so it carries "Got it" alone.  Every button also closes on `Esc`.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::tips::Tip;
use crate::app::App;
use crate::config::Theme;
use crate::ui::modal::LinkableResponse;
use crate::ui::{
    controls, ModalButton, ModalLink, ModalLinkTarget, ModalResponse, PROSE_CONTENT_WIDTH,
};

/// The off-switch button's index, named so the `resolve` arm and the button-list order can't
/// drift.  "Got it" (index 0) needs no name — it closes exactly like `Esc` and the affordance.
/// Present only on the daily tip; a browsed tip has no button at this index, and `resolve`'s
/// catch-all closes without disabling if one somehow reached it.
const DISABLE_BUTTON: usize = 1;

pub struct DailyTipModal {
    tip: &'static Tip,
    buttons: Vec<ModalButton>,
    /// Rebuilt every render so the focused link picks up its focus styling.  The stored index is
    /// the *logical* body line, not a screen row; the modal-links port re-wraps the body to map it
    /// to an on-screen rect.
    links: Vec<ModalLink>,
    chrome: ModalChrome,
}

impl DailyTipModal {
    /// The daily startup tip, carrying the "Don't show tips" off switch (see [`DISABLE_BUTTON`]).
    pub fn new(tip: &'static Tip) -> Self {
        Self::build(tip, true)
    }

    /// A tip opened on demand from the Browse-tips index: "Got it" alone, no off switch, since the
    /// reader sought it out rather than being interrupted by it.
    pub fn browsing(tip: &'static Tip) -> Self {
        Self::build(tip, false)
    }

    fn build(tip: &'static Tip, show_disable: bool) -> Self {
        let mut buttons = vec![ModalButton::new("Got it")];
        if show_disable {
            buttons.push(ModalButton::new("Don't show tips"));
        }
        Self {
            tip,
            buttons,
            links: Vec::new(),
            chrome: ModalChrome::new(ModalKind::Normal, true)
                .with_max_content_width(PROSE_CONTENT_WIDTH),
        }
    }

    /// Append the tip's "learn more" footnote — a uniform `See <page title> for more info.` with
    /// the page title as the link — and return the link list naming its span.  A no-op returning
    /// no links for a self-contained tip.  Called per render so the focused link picks up its
    /// styling; the [`ModalLink`] it returns names the footnote's *logical* body line, which the
    /// modal-links port re-wraps to a rect.
    fn append_learn_more(
        &self,
        body: &mut Vec<Line<'static>>,
        focused_link: Option<usize>,
        theme: &Theme,
    ) -> Vec<ModalLink> {
        let Some(link) = self.tip.link.as_ref() else {
            return Vec::new();
        };
        let title = link.doc.title();
        body.push(Line::raw(""));
        let line_idx = body.len();
        body.push(Line::from(vec![
            Span::raw("See "),
            Span::styled(title, controls::link_style(focused_link == Some(0), theme)),
            Span::raw(" for more info."),
        ]));
        let target = ModalLinkTarget {
            id: link.doc,
            fragment: link.fragment,
        };
        // The link is the second span (index 1) — `See ` precedes it.
        vec![ModalLink::new(line_idx, 1, target, title)]
    }

    /// Close, turning daily tips off when `disable`, and following a body link when one was
    /// activated — an overlay left floating would cover the page the reader asked for.
    fn close_with(&self, disable: bool, link: Option<ModalLinkTarget>) -> ModalOutcome {
        ModalOutcome::CloseAnd(Box::new(move |app| {
            if disable {
                app.config.editor.daily_tips = false;
                app.save_config_with_flash("failed to persist daily-tips preference");
            }
            if let Some(target) = link {
                app.follow_modal_link(target);
            }
        }))
    }

    /// Map a link-aware response, deferring non-links to [`Self::resolve`].
    fn resolve_linkable(&self, response: LinkableResponse) -> ModalOutcome {
        match response {
            LinkableResponse::Modal(r) => self.resolve(r),
            LinkableResponse::Link(idx) => match self.links.get(idx) {
                Some(link) => self.close_with(false, Some(link.target.clone())),
                None => ModalOutcome::Continue,
            },
        }
    }

    /// Map a resolved response to an outcome, shared by the key and click paths.
    fn resolve(&self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::ButtonPressed(DISABLE_BUTTON) => self.close_with(true, None),
            // "Got it", Esc, and the title-bar affordance all just close.
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => {
                self.close_with(false, None)
            }
        }
    }
}

impl Modal for DailyTipModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        // One flowing string, not pre-broken lines: `ModalView` wraps the body.
        let mut body: Vec<Line<'static>> = vec![Line::raw(self.tip.text)];
        self.links = self.append_learn_more(&mut body, self.chrome.focused_link(), ctx.theme);
        self.chrome
            .render_with_links(frame, area, ctx, "Tip", &body, &self.buttons, &self.links);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        let response = self
            .chrome
            .on_key_linkable(&key, self.links.len(), self.buttons.len());
        self.resolve_linkable(response)
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.chrome.on_wheel(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        let response = self.chrome.on_click_linkable(col, row);
        self.resolve_linkable(response)
    }

    fn kind(&self) -> ModalKind {
        self.chrome.kind()
    }

    fn dismissable(&self) -> bool {
        self.chrome.dismissable()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tips::ALL_TIPS;

    /// A tip that carries a link — the difftool tip — exercises the footnote path.
    fn linked_tip() -> &'static Tip {
        ALL_TIPS
            .iter()
            .find(|t| t.link.is_some())
            .expect("a linked tip exists")
    }

    /// A tip with no link — the cheat-sheet tip — exercises the bare path.
    fn bare_tip() -> &'static Tip {
        ALL_TIPS
            .iter()
            .find(|t| t.link.is_none())
            .expect("a link-less tip exists")
    }

    #[test]
    fn button_indices_match_their_positions() {
        // Reordering the list without updating `DISABLE_BUTTON` would silently turn the off
        // switch into a plain dismiss.
        let modal = DailyTipModal::new(linked_tip());
        assert_eq!(modal.buttons[0].label, "Got it");
        assert_eq!(modal.buttons[DISABLE_BUTTON].label, "Don't show tips");
        assert_eq!(modal.buttons.len(), 2);
    }

    #[test]
    fn a_browsed_tip_omits_the_off_switch() {
        // The Browse-tips path was sought out, so it offers "Got it" alone — no button at
        // `DISABLE_BUTTON`, and thus no way to disable tips from a tip the reader chose to open.
        let modal = DailyTipModal::browsing(linked_tip());
        assert_eq!(modal.buttons.len(), 1);
        assert_eq!(modal.buttons[0].label, "Got it");
    }

    #[test]
    fn a_linked_tip_appends_a_see_more_footnote_and_a_bare_one_does_not() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));

        let mut body = Vec::new();
        let links = DailyTipModal::new(linked_tip()).append_learn_more(&mut body, None, theme);
        assert_eq!(
            links.len(),
            1,
            "a linked tip gets exactly one footnote link"
        );
        // Spacer + footnote line; the footnote reads "See <title> for more info.", with the
        // page title as the (second) span so it can be the link.
        let footnote: String = body
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            footnote,
            format!(
                "See {} for more info.",
                linked_tip().link.as_ref().unwrap().doc.title()
            ),
        );
        assert_eq!(links[0].span_idx, 1, "the title span, after `See `");

        let mut body = Vec::new();
        let links = DailyTipModal::new(bare_tip()).append_learn_more(&mut body, None, theme);
        assert!(links.is_empty(), "a self-contained tip adds no footnote");
        assert!(body.is_empty());
    }

    #[test]
    fn the_disable_button_turns_daily_tips_off() {
        let _iso = crate::test_env::config_isolation();
        let mut app = crate::app::test_utils::make_app();
        assert!(app.config.editor.daily_tips, "on by default");
        let modal = DailyTipModal::new(bare_tip());
        let outcome = modal.resolve(ModalResponse::ButtonPressed(DISABLE_BUTTON));
        match outcome {
            ModalOutcome::CloseAnd(f) => f(&mut app),
            _ => panic!("the off switch closes the modal"),
        }
        assert!(!app.config.editor.daily_tips);
    }

    #[test]
    fn got_it_closes_without_changing_the_setting() {
        let _iso = crate::test_env::config_isolation();
        let mut app = crate::app::test_utils::make_app();
        let modal = DailyTipModal::new(bare_tip());
        let outcome = modal.resolve(ModalResponse::ButtonPressed(0));
        match outcome {
            ModalOutcome::CloseAnd(f) => f(&mut app),
            _ => panic!("Got it closes"),
        }
        assert!(app.config.editor.daily_tips, "unchanged");
    }
}

/// End-to-end coverage for a linked tip's footnote: render, hit-test the rect the render
/// recorded, and follow it into a real `App`.  The one test proving the footnote geometry a click
/// is matched against is the geometry the renderer produced.
#[cfg(test)]
mod click_tests {
    use super::*;
    use crate::app::test_utils::make_app;
    use crate::app::tips::ALL_TIPS;
    use crate::config::{Config, Theme};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn clicking_a_tips_footnote_opens_that_manual_page() {
        let _iso = crate::test_env::config_isolation();
        let mut app = make_app();
        let tip = ALL_TIPS
            .iter()
            .find(|t| t.link.is_some())
            .expect("a linked tip exists");
        let expected = tip.link.as_ref().unwrap().doc;
        let mut modal = DailyTipModal::new(tip);
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let config = Config::default();

        // A real render is what populates `link_rects`.
        let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
        term.draw(|frame| {
            let ctx = ModalRenderCtx {
                theme,
                config: &config,
                cursor_visible: false,
            };
            let area = frame.area();
            modal.render(frame, area, &ctx);
        })
        .unwrap();

        let (_, rect) = *modal
            .chrome
            .state
            .link_rects
            .first()
            .expect("the rendered body records a rect for the footnote link");
        match modal.handle_click(rect.x, rect.y, &mut app) {
            ModalOutcome::CloseAnd(f) => f(&mut app),
            _ => panic!("a footnote click closes the tip and follows the link"),
        }
        assert_eq!(
            app.open_doc,
            Some(expected),
            "opened the page the link named"
        );
    }
}
