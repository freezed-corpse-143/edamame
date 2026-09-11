//! New-terminal capabilities notice, fired the first time edamame runs in a
//! terminal whose [`fingerprint`](crate::terminal::Capabilities::fingerprint)
//! isn't in `editor.seen_terminal_fingerprints`.  Dismissing records the
//! fingerprint, so it stays quiet here but re-fires somewhere new.
//!
//! A new terminal can be *worse* than the last, so the notice is not purely
//! informational: "Adjust settings" stacks the welcome modal — the one surface
//! that re-derives `full_color` / `image_capable` from the live capabilities.
//! The generic settings overlay is deliberately not the destination; it would
//! let the user enable images on a terminal that quantizes them.
//!
//! When the startup indexed-color theme substitution fires on the same launch,
//! this notice absorbs its explanation via
//! [`with_theme_downgrade`](TerminalCapabilitiesModal::with_theme_downgrade) and
//! `App::new` suppresses the standalone [`super::ThemeDowngradeModal`] — two
//! modals about the same terminal, one hiding the other, reads as a bug.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::Frame;

use super::chrome::ModalChrome;
use super::docs_link::DocsFootnote;
use super::types::{Modal, ModalKind, ModalOutcome, ModalRenderCtx};
use crate::app::App;
use crate::docs::DocId;
use crate::terminal::Capabilities;
use crate::ui::modal::LinkableResponse;
use crate::ui::{
    build_cap_lines, theme_downgrade_lines, CapSummary, ModalButton, ModalLink, ModalLinkTarget,
    ModalResponse, PROSE_CONTENT_WIDTH,
};

/// Index of the "Adjust settings" button, named so the `resolve` match arm and
/// the button-list order can't drift.
const ADJUST_BUTTON: usize = 0;

/// The manual page this notice points at.
///
/// The one footnote in the crate carrying no fragment, deliberately: the whole
/// page is about what this modal just reported, and nothing here knows which of
/// the five rows the reader cares about.
const DOCS_FOOTNOTE: DocsFootnote = DocsFootnote {
    label: "Terminal compatibility",
    target: ModalLinkTarget {
        id: DocId::TerminalCompatibility,
        fragment: None,
    },
    trailer: " lists what each capability affects and which terminals support it.",
};

/// The startup theme substitution, when it fired on this launch.  See
/// [`crate::ui::theme_downgrade_lines`].
struct ThemeDowngrade {
    configured: String,
    substituted: &'static str,
}

pub struct TerminalCapabilitiesModal {
    summary: CapSummary,
    fingerprint: String,
    downgrade: Option<ThemeDowngrade>,
    buttons: Vec<ModalButton>,
    /// Rebuilt every render: the link's coordinates depend on how many optional
    /// paragraphs precede it.
    links: Vec<ModalLink>,
    chrome: ModalChrome,
}

impl TerminalCapabilitiesModal {
    /// Build the modal, or `None` when this terminal's fingerprint is already in
    /// the seen set.
    pub fn from_capabilities(caps: &Capabilities, seen: &[String]) -> Option<Self> {
        let fingerprint = caps.fingerprint();
        if seen.iter().any(|s| s == &fingerprint) {
            return None;
        }
        Some(Self {
            summary: CapSummary::from_caps(caps),
            fingerprint,
            downgrade: None,
            buttons: vec![ModalButton::new("Adjust settings")],
            links: Vec::new(),
            // Capped at the width the standalone downgrade modal uses, so the
            // two stay visually interchangeable; the rows are narrower anyway.
            chrome: ModalChrome::new(ModalKind::Normal, true)
                .with_max_content_width(PROSE_CONTENT_WIDTH),
        })
    }

    /// Fold the startup theme substitution's explanation into this notice, as
    /// `App::new` does instead of pushing a separate
    /// [`super::ThemeDowngradeModal`] when both would fire on one launch.
    ///
    /// Also drops the "Adjust settings" button: a downgrade means no 24-bit
    /// color, where the welcome modal has only the vim toggle left to offer —
    /// and saving it would *persist* its forced-off values over whatever the
    /// user chose on a capable terminal.  `Action::OpenWelcome` still reaches it
    /// deliberately.
    pub fn with_theme_downgrade(mut self, configured: String, substituted: &'static str) -> Self {
        self.downgrade = Some(ThemeDowngrade {
            configured,
            substituted,
        });
        self.buttons.clear();
        self
    }

    /// Close, record the fingerprint as seen, and — when `adjust` — stack the
    /// welcome modal.  Recording happens on both paths: the notice has served
    /// its purpose either way.
    fn record_outcome(&self, adjust: bool) -> ModalOutcome {
        self.record_outcome_following(adjust, None)
    }

    /// [`Self::record_outcome`], optionally following a body link on the way
    /// out.  Doing so records the fingerprint too, or the notice re-fires next
    /// launch purely because the user read the manual; and it closes rather than
    /// float over the page the reader just asked for.
    fn record_outcome_following(
        &self,
        adjust: bool,
        link: Option<ModalLinkTarget>,
    ) -> ModalOutcome {
        let fp = self.fingerprint.clone();
        ModalOutcome::CloseAnd(Box::new(move |app| {
            if !app.config.editor.seen_terminal_fingerprints.contains(&fp) {
                app.config.editor.seen_terminal_fingerprints.push(fp);
                app.save_config_with_flash("failed to persist terminal capabilities notice");
            }
            if adjust {
                app.open_welcome_modal();
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
                Some(link) => self.record_outcome_following(false, Some(link.target.clone())),
                None => ModalOutcome::Continue,
            },
        }
    }

    /// Map a resolved response to an outcome, shared by the key and click
    /// paths.  There is deliberately no Dismiss button: per convention a
    /// dismissable modal closes with `Esc` or the title-bar affordance.
    fn resolve(&self, response: ModalResponse) -> ModalOutcome {
        match response {
            ModalResponse::Continue => ModalOutcome::Continue,
            ModalResponse::ButtonPressed(ADJUST_BUTTON) => self.record_outcome(true),
            ModalResponse::Cancelled | ModalResponse::ButtonPressed(_) => {
                self.record_outcome(false)
            }
        }
    }
}

impl Modal for TerminalCapabilitiesModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        // Paragraphs, not pre-broken lines: `ModalView` wraps the body.
        let mut body: Vec<Line<'static>> = Vec::new();
        body.push(Line::raw(
            "It looks like you're using a new terminal application. edamame has \
             detected the following capabilities for this terminal:",
        ));
        body.push(Line::raw(""));
        body.extend(build_cap_lines(&self.summary.rows, ctx.theme));
        if !self.summary.all_ok() {
            body.push(Line::raw(""));
            body.push(Line::styled(
                "Items marked ✗ will be disabled or degraded automatically.",
                Style::default().fg(ctx.theme.palette.warning),
            ));
        }
        if let Some(d) = &self.downgrade {
            body.push(Line::raw(""));
            body.extend(theme_downgrade_lines(
                &d.configured,
                d.substituted,
                ctx.theme,
            ));
        }
        // The trailer describes the button, so it goes when the button does.
        if self.downgrade.is_none() {
            body.push(Line::raw(""));
            body.push(Line::raw(
                "Adjust settings re-opens the setup screen so theme, images, and diagrams \
                 can be matched to this terminal.",
            ));
        }
        // Appended through the shared helper so its line index is observed
        // rather than assumed — the optional paragraphs above shift it.
        self.links = DOCS_FOOTNOTE.append_to(&mut body, self.chrome.focused_link(), ctx.theme);
        self.chrome.render_with_links(
            frame,
            area,
            ctx,
            "Terminal capabilities",
            &body,
            &self.buttons,
            &self.links,
        );
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
    use crate::config::Theme;
    use crate::document::ParsedDoc;
    use crossterm::event::KeyCode;

    #[test]
    fn a_folded_downgrade_drops_the_welcome_route() {
        // See `with_theme_downgrade`.
        let modal = TerminalCapabilitiesModal::from_capabilities(&Capabilities::minimal(), &[])
            .expect("unseen fingerprint yields a modal")
            .with_theme_downgrade("Dracula".into(), "256 Dark");
        assert!(modal.buttons.is_empty());
    }

    #[test]
    fn adjust_button_index_matches_its_position() {
        // Reordering the list without updating the const would silently turn
        // "Adjust settings" into a plain dismiss.
        let modal = TerminalCapabilitiesModal::from_capabilities(&Capabilities::minimal(), &[])
            .expect("unseen fingerprint yields a modal");
        assert_eq!(modal.buttons[ADJUST_BUTTON].label, "Adjust settings");
        assert_eq!(modal.buttons.len(), 1);
    }

    /// The footnote must name a page that is really embedded, and must stay
    /// fragment-less (see `DOCS_FOOTNOTE`) — adding one would silently change
    /// where a reader lands without failing anything else.
    #[test]
    fn the_docs_link_names_a_page_that_exists() {
        let ModalLinkTarget { id, fragment } = DOCS_FOOTNOTE.target;
        assert_eq!(
            fragment, None,
            "this notice points at the page top; see DOCS_FOOTNOTE"
        );
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let parsed = ParsedDoc::build(&id.source(), theme, true, 80);
        assert!(!parsed.blocks.is_empty(), "{} embedded empty", id.title());
    }

    #[test]
    fn tab_walks_links_before_buttons() {
        let mut modal = TerminalCapabilitiesModal::from_capabilities(&Capabilities::minimal(), &[])
            .expect("unseen fingerprint yields a modal");
        let tab = KeyEvent::new(KeyCode::Tab, crossterm::event::KeyModifiers::NONE);
        assert_eq!(
            modal.chrome.focused_link(),
            None,
            "buttons hold focus first"
        );
        modal.chrome.on_key_linkable(&tab, 1, 1);
        assert_eq!(modal.chrome.focused_link(), Some(0), "Tab reaches the link");
        modal.chrome.on_key_linkable(&tab, 1, 1);
        assert_eq!(modal.chrome.focused_link(), None, "and wraps back");
    }

    #[test]
    fn enter_on_the_focused_link_resolves_as_a_link() {
        let mut modal = TerminalCapabilitiesModal::from_capabilities(&Capabilities::minimal(), &[])
            .expect("unseen fingerprint yields a modal");
        let tab = KeyEvent::new(KeyCode::Tab, crossterm::event::KeyModifiers::NONE);
        modal.chrome.on_key_linkable(&tab, 1, 1);
        let enter = KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE);
        let response = modal.chrome.on_key_linkable(&enter, 1, 1);
        assert_eq!(response, LinkableResponse::Link(0));
    }

    /// Enter on a focused link must not fall through to the button the ring last
    /// sat on.
    #[test]
    fn enter_on_a_link_does_not_press_the_button() {
        let mut modal = TerminalCapabilitiesModal::from_capabilities(&Capabilities::minimal(), &[])
            .expect("unseen fingerprint yields a modal");
        let tab = KeyEvent::new(KeyCode::Tab, crossterm::event::KeyModifiers::NONE);
        modal.chrome.on_key_linkable(&tab, 1, 1);
        let enter = KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE);
        let response = modal.chrome.on_key_linkable(&enter, 1, 1);
        assert!(
            !matches!(
                response,
                LinkableResponse::Modal(ModalResponse::ButtonPressed(_))
            ),
            "a focused link swallows Enter"
        );
    }

    #[test]
    fn seen_fingerprint_suppresses_the_notice() {
        let caps = Capabilities::minimal();
        let seen = vec![caps.fingerprint()];
        assert!(TerminalCapabilitiesModal::from_capabilities(&caps, &seen).is_none());
    }
}

/// End-to-end coverage for the body link: render, hit-test the rect the render
/// recorded, and follow it into a real `App` — the only test proving the
/// geometry the renderer produced is what a click is matched against.  Separate
/// because it needs an `App` and a drawn frame.
#[cfg(test)]
mod click_tests {
    use super::*;
    use crate::app::test_utils::make_app;
    use crate::config::Config;
    use crate::config::Theme;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn clicking_the_docs_link_opens_that_manual_page() {
        // The link path records the fingerprint, which reaches `Config::save`.
        let _iso = crate::test_env::config_isolation();
        let mut app = make_app();
        let mut modal = TerminalCapabilitiesModal::from_capabilities(&Capabilities::minimal(), &[])
            .expect("unseen fingerprint yields a modal");
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
            .expect("the rendered body records a rect for its link");
        let outcome = modal.handle_click(rect.x, rect.y, &mut app);
        match outcome {
            ModalOutcome::CloseAnd(f) => f(&mut app),
            _ => panic!("a link click closes the notice and follows the link"),
        }

        assert_eq!(
            app.open_doc,
            Some(DOCS_FOOTNOTE.target.id),
            "the click opened the manual page the link named"
        );
        // Recorded on the link path too, or the notice re-fires next launch.
        assert!(app
            .config
            .editor
            .seen_terminal_fingerprints
            .contains(&Capabilities::minimal().fingerprint()));
    }

    /// A click missing every link must still resolve the modal as before.
    #[test]
    fn a_click_outside_the_link_does_not_navigate() {
        let _iso = crate::test_env::config_isolation();
        let mut app = make_app();
        let mut modal = TerminalCapabilitiesModal::from_capabilities(&Capabilities::minimal(), &[])
            .expect("unseen fingerprint yields a modal");
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let config = Config::default();
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

        // (0, 0) is outside the centered modal entirely.
        let outcome = modal.handle_click(0, 0, &mut app);
        if let ModalOutcome::CloseAnd(f) = outcome {
            f(&mut app);
        }
        assert_eq!(app.open_doc, None);
    }
}
