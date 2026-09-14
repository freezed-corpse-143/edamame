//! "Browse tips" modal — a plain list of every daily tip ([`crate::app::tips`]), reached from the
//! command palette.  Selecting one opens that tip's [`super::DailyTipModal`] on top; closing it
//! returns here, so a user can read several in a row.
//!
//! Built on the shared [`SearchableList`] like the section picker.  It keeps that component's
//! search field — a small consistency win, and harmless over a short list — rather than a bespoke
//! no-search widget; the rows are plain titles either way.  Rendering stays in this (app) layer
//! rather than a `ui/` helper because the item is a `&'static Tip`, and `ui` sits below `app`.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use super::types::{esc_rect_hit, Modal, ModalOutcome, ModalRenderCtx};
use crate::app::tips::{Tip, ALL_TIPS};
use crate::app::App;
use crate::config::Theme;
use crate::ui::modal_row::truncate_to_cells;
use crate::ui::searchable_list::{
    draw_searchable_list_modal, ListEvent, ListModalOpts, RowCtx, SearchableList,
};

const PLACEHOLDER: &str = "Type to filter tips…";

/// Blank rows above and below the list on a terminal tall enough to spare them.
const VERTICAL_PAD: u16 = 4;

/// Below this height the list drops its padding and grows edge-to-edge.
const SHORT_TERMINAL_ROWS: u16 = 20;

/// Width floor so the frame doesn't snap narrower than the placeholder.
const MIN_CONTENT_WIDTH: u16 = 24;

/// Fuzzy-match (and label) each row on its title.
fn tip_title<'a>(t: &'a &'static Tip) -> &'a str {
    t.title
}

pub struct TipsIndexModal {
    list: SearchableList<&'static Tip>,
    /// Cached `esc` close-hint rect for click hit-testing; set each render.
    esc_button_rect: Option<Rect>,
}

impl TipsIndexModal {
    pub fn new() -> Self {
        Self {
            list: SearchableList::new(ALL_TIPS.iter().collect(), tip_title),
            esc_button_rect: None,
        }
    }

    /// Map a list event to an outcome.  A submission stacks the tip on top of this index
    /// ([`ModalOutcome::ContinueAnd`]) so closing it returns here; there is no live preview, so a
    /// focus change is inert.
    fn outcome_for(&self, event: ListEvent) -> ModalOutcome {
        match event {
            ListEvent::Continue | ListEvent::FocusChanged(_) => ModalOutcome::Continue,
            ListEvent::Cancelled => ModalOutcome::Close,
            ListEvent::Submitted(i) => {
                let tip: &'static Tip = self.list.items()[i];
                ModalOutcome::ContinueAnd(Box::new(move |app| app.open_tip(tip)))
            }
        }
    }
}

impl Default for TipsIndexModal {
    fn default() -> Self {
        Self::new()
    }
}

impl Modal for TipsIndexModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        // Keep the list off the hint line + status bar, as the section picker does.
        let bottom_rows = crate::ui::BottomRegion::height();
        let area = Rect {
            height: area.height.saturating_sub(bottom_rows),
            ..area
        };
        let vertical_pad = if area.height < SHORT_TERMINAL_ROWS {
            0
        } else {
            VERTICAL_PAD
        };
        let content_width = self
            .list
            .items()
            .iter()
            .map(|t| UnicodeWidthStr::width(t.title) as u16)
            .max()
            .unwrap_or(0)
            .saturating_add(4) // marker + right margin
            .max(MIN_CONTENT_WIDTH)
            .max(PLACEHOLDER.chars().count() as u16 + 2);
        let theme = ctx.theme;
        self.esc_button_rect = draw_searchable_list_modal(
            &mut self.list,
            area,
            frame.buffer_mut(),
            ListModalOpts {
                title: "Browse Tips",
                content_width,
                max_list_rows: u16::MAX,
                vertical_pad,
                theme,
                cursor_visible: ctx.cursor_visible,
                placeholder: PLACEHOLDER,
                empty_text: "(no matches)",
            },
            |row| match row {
                RowCtx::Item {
                    item,
                    focused,
                    width,
                } => format_tip_row(item, focused, theme, width),
                // The tips index never builds header rows.
                RowCtx::Header { title, .. } => {
                    Line::from(Span::styled(title.to_owned(), theme.modal_item))
                }
            },
        );
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        _app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        let event = self.list.handle_key(&key);
        self.outcome_for(event)
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        let event = self.list.paste(text);
        self.outcome_for(event)
    }

    fn handle_wheel(&mut self, delta: i32) {
        self.list.scroll_by(delta);
    }

    fn handle_click(&mut self, col: u16, row: u16, _app: &mut App) -> ModalOutcome {
        if esc_rect_hit(self.esc_button_rect, col, row) {
            return ModalOutcome::Close;
        }
        let event = self.list.handle_click(col, row);
        self.outcome_for(event)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// One tip row: the palette's selection styling when focused, else a muted item style.
fn format_tip_row(tip: &Tip, focused: bool, theme: &Theme, width: u16) -> Line<'static> {
    let marker = if focused { "› " } else { "  " };
    let text_budget = (width as usize).saturating_sub(UnicodeWidthStr::width(marker));
    let label = format!("{marker}{}", truncate_to_cells(tip.title, text_budget));
    if focused {
        let pad = (width as usize).saturating_sub(UnicodeWidthStr::width(label.as_str()));
        Line::from(Span::styled(
            format!("{label}{}", " ".repeat(pad)),
            theme.modal_item_selected,
        ))
    } else {
        Line::from(Span::styled(label, theme.modal_item))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_utils::make_app;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn the_index_lists_every_tip() {
        let modal = TipsIndexModal::new();
        assert_eq!(modal.list.items().len(), ALL_TIPS.len());
    }

    #[test]
    fn enter_opens_the_focused_tip_and_keeps_the_index() {
        let _iso = crate::test_env::config_isolation();
        let mut app = make_app();
        let mut modal = TipsIndexModal::new();
        // First row is tip id 1.
        match modal.handle_key(key(KeyCode::Enter), &mut app, 0, 0) {
            ModalOutcome::ContinueAnd(f) => f(&mut app),
            _ => panic!("Enter should stack a tip on top of the index"),
        }
        assert!(app.modal_stack.contains::<super::super::DailyTipModal>());
    }

    #[test]
    fn esc_closes_the_index() {
        let mut app = make_app();
        let mut modal = TipsIndexModal::new();
        assert!(matches!(
            modal.handle_key(key(KeyCode::Esc), &mut app, 0, 0),
            ModalOutcome::Close
        ));
    }

    #[test]
    fn filtering_narrows_the_list() {
        let mut modal = TipsIndexModal::new();
        for c in "vim".chars() {
            modal.list.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(modal.list.match_count(), 1, "only the Vim mode tip matches");
    }
}
