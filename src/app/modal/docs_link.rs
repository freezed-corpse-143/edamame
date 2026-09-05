//! The "see the manual" footnote a modal appends to its body.  Built through one helper so
//! the link's `(line, span)` coordinates are observed from the finished body rather than
//! assumed by a modal with optional paragraphs.
//!
//! Only informational modals carry one: on a question prompt (images / diagrams /
//! remote-image) following a link closes the prompt, and closing *is* an answer.

use ratatui::text::{Line, Span};

use super::types::ModalOutcome;
use crate::config::Theme;
use crate::ui::{controls, ModalLink, ModalLinkTarget};

/// A one-line pointer into the manual, appended below a modal's body.
pub(super) struct DocsFootnote {
    /// The clickable text; should match the target section's heading.
    pub label: &'static str,
    pub target: ModalLinkTarget,
    /// The rest of the sentence after `label`; begin it with a space.
    pub trailer: &'static str,
}

impl DocsFootnote {
    /// Append a spacer and the footnote line to `body`; returns the link list naming the
    /// span just written.  `focused_link` is [`super::chrome::ModalChrome::focused_link`].
    pub(super) fn append_to(
        &self,
        body: &mut Vec<Line<'static>>,
        focused_link: Option<usize>,
        theme: &Theme,
    ) -> Vec<ModalLink> {
        body.push(Line::raw(""));
        let line_idx = body.len();
        body.push(Line::from(vec![
            Span::styled(
                self.label,
                controls::link_style(focused_link == Some(0), theme),
            ),
            Span::raw(self.trailer),
        ]));
        vec![ModalLink::new(line_idx, 0, self.target.clone(), self.label)]
    }
}

/// Close the modal and follow `target` — an overlay left floating would cover the page the
/// reader asked for.  A modal with its own dismissal bookkeeping builds its own outcome.
pub(super) fn follow_and_close(target: ModalLinkTarget) -> ModalOutcome {
    ModalOutcome::CloseAnd(Box::new(move |app| app.follow_modal_link(target)))
}
