//! Diff-mode intro modal: a scrollable explanation with a pinned footer holding a "Don't show
//! this again" toggle above a `[ Continue ]` button.  Built on the `scroll_container` primitives
//! rather than [`crate::ui::ModalView`] because that layout cannot express a pinned control row
//! above the buttons.  The adapter `src/app/modal/diff_intro.rs` supplies the body and persists
//! the opt-out.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget, Wrap},
};

use crate::config::Theme;
use crate::ui::button_row::{button_row_width, render_button_row};
use crate::ui::controls;
use crate::ui::scroll_container::{
    centered_rect_for_content, compute_pad_h, draw_frame, wrapped_rows, ContentSize, FrameOpts,
    ModalKind, ScrollContainerState, MAX_PAD_H, VERTICAL_CHROME_ROWS,
};

const TOGGLE_LABEL: &str = "Don't show this again";
/// Cells between the toggle label and the slider.
const TOGGLE_LABEL_GAP: usize = 2;
const CONTINUE_LABEL: &str = "Continue";

/// The two focus targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffIntroFocus {
    Toggle,
    Confirm,
}

impl DiffIntroFocus {
    fn flipped(self) -> Self {
        match self {
            Self::Toggle => Self::Confirm,
            Self::Confirm => Self::Toggle,
        }
    }
}

/// Outcome of dispatching an event to the diff-intro modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffIntroResponse {
    /// Modal stays open; the caller just redraws.
    Continue,
    /// Close; the caller reads [`DiffIntroState::dont_show_again`] to persist the opt-out.
    Close,
}

/// Mutable state for an open diff-intro modal.
#[derive(Debug, Clone)]
pub struct DiffIntroState {
    pub focus: DiffIntroFocus,
    /// `true` persists `config.editor.show_diff_intro = false` on close.
    pub dont_show_again: bool,
    pub scroll_state: ScrollContainerState,
    /// Click hit-rects, captured each render.
    pub toggle_rect: Option<Rect>,
    pub continue_rect: Option<Rect>,
    pub esc_button_rect: Option<Rect>,
}

impl Default for DiffIntroState {
    fn default() -> Self {
        Self::new()
    }
}

impl DiffIntroState {
    pub fn new() -> Self {
        Self {
            // Continue focused by default so a bare Enter proceeds.
            focus: DiffIntroFocus::Confirm,
            dont_show_again: false,
            scroll_state: ScrollContainerState::default(),
            toggle_rect: None,
            continue_rect: None,
            esc_button_rect: None,
        }
    }

    /// Apply a key event: paging keys scroll the body, Up / Down / Tab move focus, Left / Right
    /// flip the toggle (or move focus off Continue), Enter / Space / `y` activate, Esc / `n` close.
    pub fn handle_key(&mut self, key: &KeyEvent) -> DiffIntroResponse {
        if self.scroll_state.handle_paging_key(key) {
            return DiffIntroResponse::Continue;
        }
        let none = key.modifiers == KeyModifiers::NONE;
        match key.code {
            KeyCode::Esc => DiffIntroResponse::Close,
            KeyCode::Char('n') | KeyCode::Char('N') if none => DiffIntroResponse::Close,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                self.focus = self.focus.flipped();
                DiffIntroResponse::Continue
            }
            KeyCode::Left | KeyCode::Right => {
                if self.focus == DiffIntroFocus::Toggle {
                    self.dont_show_again = !self.dont_show_again;
                } else {
                    self.focus = self.focus.flipped();
                }
                DiffIntroResponse::Continue
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.activate(),
            KeyCode::Char('y') | KeyCode::Char('Y') if none => self.activate(),
            _ => DiffIntroResponse::Continue,
        }
    }

    fn activate(&mut self) -> DiffIntroResponse {
        match self.focus {
            DiffIntroFocus::Toggle => {
                self.dont_show_again = !self.dont_show_again;
                DiffIntroResponse::Continue
            }
            DiffIntroFocus::Confirm => DiffIntroResponse::Close,
        }
    }

    pub fn handle_wheel(&mut self, delta: i32) {
        self.scroll_state.scroll_by(delta);
    }

    /// Hit-test a left-click: the toggle row flips (and focuses) the toggle; Continue or the
    /// `esc` hint closes.
    pub fn handle_click(&mut self, col: u16, row: u16) -> DiffIntroResponse {
        if rect_contains(self.toggle_rect, col, row) {
            self.focus = DiffIntroFocus::Toggle;
            self.dont_show_again = !self.dont_show_again;
            return DiffIntroResponse::Continue;
        }
        if rect_contains(self.continue_rect, col, row)
            || rect_contains(self.esc_button_rect, col, row)
        {
            return DiffIntroResponse::Close;
        }
        DiffIntroResponse::Continue
    }
}

fn rect_contains(rect: Option<Rect>, col: u16, row: u16) -> bool {
    rect.is_some_and(|r| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
}

/// View widget, drawn each frame from the adapter-supplied body lines.
pub struct DiffIntroView<'a> {
    pub theme: &'a Theme,
    pub body: &'a [Line<'a>],
}

impl<'a> DiffIntroView<'a> {
    /// The toggle group (label + slider) and its rendered width.
    fn toggle_line(&self, state: &DiffIntroState) -> (Line<'static>, u16) {
        let focused = state.focus == DiffIntroFocus::Toggle;
        let label_style = controls::control_label_style(focused, false, self.theme);
        let mut spans = vec![Span::styled(
            format!("{TOGGLE_LABEL}{}", " ".repeat(TOGGLE_LABEL_GAP)),
            label_style,
        )];
        spans.extend(controls::toggle_spans(
            state.dont_show_again,
            focused,
            false,
            self.theme,
        ));
        let width = (TOGGLE_LABEL.chars().count() + TOGGLE_LABEL_GAP) as u16
            + controls::toggle_width() as u16;
        (Line::from(spans), width)
    }
}

impl<'a> StatefulWidget for DiffIntroView<'a> {
    type State = DiffIntroState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let (toggle_line, toggle_w) = self.toggle_line(state);
        let continue_w = button_row_width(&[CONTINUE_LABEL]);
        let body_w = self.body.iter().map(|l| l.width()).max().unwrap_or(0) as u16;
        let content_width = body_w.max(toggle_w).max(continue_w);

        // Blank spacer + toggle row + spacer + Continue row.
        let pinned_bottom: u16 = 4;

        // Same wrap-width derivation as `ModalView`, so pre-render height matches the real wrap.
        let prospective_modal_width = content_width.saturating_add(2 * MAX_PAD_H).min(area.width);
        let prospective_pad_h = compute_pad_h(prospective_modal_width, content_width, MAX_PAD_H);
        let prospective_inner_w = prospective_modal_width
            .saturating_sub(2 * prospective_pad_h)
            .max(1);
        let wrapped_body_height = wrapped_rows(self.body, prospective_inner_w);

        let content = ContentSize {
            width: content_width,
            height: wrapped_body_height,
            pinned_top: 0,
            pinned_bottom,
            max_pad_h: MAX_PAD_H,
        };
        let modal_area = centered_rect_for_content(content, area);

        let body_inner_h = modal_area.height.saturating_sub(VERTICAL_CHROME_ROWS);
        let text_body_height = body_inner_h.saturating_sub(pinned_bottom);
        let pad_h = compute_pad_h(modal_area.width, content_width, MAX_PAD_H);
        let body_inner_w = modal_area.width.saturating_sub(2 * pad_h).max(1);
        let total = wrapped_rows(self.body, body_inner_w);
        state.scroll_state.observe(total, text_body_height);

        let layout = draw_frame(
            modal_area,
            buf,
            FrameOpts {
                title: "Entering diff mode",
                kind: ModalKind::Normal,
                show_close_hint: true,
                content,
                theme: self.theme,
            },
        );
        state.esc_button_rect = layout.esc_hit_rect;
        let inner = layout.body;
        if inner.height == 0 || inner.width == 0 {
            state.toggle_rect = None;
            state.continue_rect = None;
            return;
        }

        // ── Scrollable body ─────────────────────────────────────────────
        let body_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: text_body_height,
        };
        Paragraph::new(self.body.to_vec())
            .wrap(Wrap { trim: false })
            .style(self.theme.modal_bg)
            .scroll((state.scroll_state.scroll, 0))
            .render(body_area, buf);
        if state.scroll_state.max_scroll() > 0 {
            let bar_area = Rect {
                x: layout.scrollbar_col,
                y: body_area.y,
                width: 1,
                height: body_area.height,
            };
            crate::ui::scrollbar::render_for_scroll_state(
                bar_area,
                &state.scroll_state,
                self.theme,
                buf,
            );
        }

        // ── Pinned footer: blank, toggle row, spacer, Continue button ────
        let blank_y = inner.y + text_body_height;
        Paragraph::new("").style(self.theme.modal_bg).render(
            Rect {
                x: inner.x,
                y: blank_y,
                width: inner.width,
                height: 1,
            },
            buf,
        );
        let footer_y = blank_y + 1;
        let toggle_x = inner
            .x
            .saturating_add(inner.width.saturating_sub(toggle_w) / 2);
        let row_fill = Rect {
            x: inner.x,
            y: footer_y,
            width: inner.width,
            height: 1,
        };
        Paragraph::new("")
            .style(self.theme.modal_bg)
            .render(row_fill, buf);
        Paragraph::new(toggle_line)
            .style(self.theme.modal_bg)
            .render(
                Rect {
                    x: toggle_x,
                    y: footer_y,
                    width: toggle_w,
                    height: 1,
                },
                buf,
            );
        state.toggle_rect = Some(Rect {
            x: toggle_x,
            y: footer_y,
            width: toggle_w,
            height: 1,
        });

        let continue_area = Rect {
            x: inner.x,
            y: footer_y + 2,
            width: inner.width,
            height: 1,
        };
        let focused_idx = if state.focus == DiffIntroFocus::Confirm {
            0
        } else {
            usize::MAX
        };
        let rects = render_button_row(
            continue_area,
            buf,
            &[CONTINUE_LABEL],
            focused_idx,
            self.theme,
        );
        state.continue_rect = rects.into_iter().next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use ratatui::{backend::TestBackend, Terminal};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn body() -> Vec<Line<'static>> {
        vec![Line::raw("Diff mode explanation."), Line::raw("More text.")]
    }

    fn render(state: &mut DiffIntroState) {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let body = body();
        let mut terminal = Terminal::new(TestBackend::new(70, 20)).unwrap();
        terminal
            .draw(|frame| {
                let view = DiffIntroView { theme, body: &body };
                frame.render_stateful_widget(view, frame.area(), state);
            })
            .unwrap();
    }

    #[test]
    fn focus_starts_on_continue() {
        assert_eq!(DiffIntroState::new().focus, DiffIntroFocus::Confirm);
    }

    #[test]
    fn arrows_and_tab_move_focus_between_targets() {
        let mut s = DiffIntroState::new();
        s.handle_key(&key(KeyCode::Up));
        assert_eq!(s.focus, DiffIntroFocus::Toggle);
        s.handle_key(&key(KeyCode::Down));
        assert_eq!(s.focus, DiffIntroFocus::Confirm);
        s.handle_key(&key(KeyCode::Tab));
        assert_eq!(s.focus, DiffIntroFocus::Toggle);
    }

    #[test]
    fn activating_toggle_flips_and_keeps_modal_open() {
        let mut s = DiffIntroState::new();
        s.handle_key(&key(KeyCode::Up)); // focus toggle
        assert!(!s.dont_show_again);
        let r = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(r, DiffIntroResponse::Continue);
        assert!(s.dont_show_again);
        let r = s.handle_key(&key(KeyCode::Char(' ')));
        assert_eq!(r, DiffIntroResponse::Continue);
        assert!(!s.dont_show_again);
    }

    #[test]
    fn left_right_flip_toggle_when_focused() {
        let mut s = DiffIntroState::new();
        s.handle_key(&key(KeyCode::Up)); // focus toggle
        s.handle_key(&key(KeyCode::Right));
        assert!(s.dont_show_again);
        s.handle_key(&key(KeyCode::Left));
        assert!(!s.dont_show_again);
    }

    #[test]
    fn enter_on_continue_closes() {
        let mut s = DiffIntroState::new();
        assert_eq!(s.handle_key(&key(KeyCode::Enter)), DiffIntroResponse::Close);
    }

    #[test]
    fn esc_closes() {
        let mut s = DiffIntroState::new();
        assert_eq!(s.handle_key(&key(KeyCode::Esc)), DiffIntroResponse::Close);
    }

    #[test]
    fn clicking_toggle_then_continue_closes() {
        let mut s = DiffIntroState::new();
        render(&mut s);
        let toggle = s.toggle_rect.expect("toggle rect populated");
        let r = s.handle_click(toggle.x, toggle.y);
        assert_eq!(r, DiffIntroResponse::Continue);
        assert!(s.dont_show_again, "toggle click must flip the opt-out");

        render(&mut s);
        let cont = s.continue_rect.expect("continue rect populated");
        assert_eq!(
            s.handle_click(cont.x + cont.width / 2, cont.y),
            DiffIntroResponse::Close,
        );
    }

    #[test]
    fn click_outside_controls_keeps_modal_open() {
        let mut s = DiffIntroState::new();
        render(&mut s);
        assert_eq!(s.handle_click(0, 0), DiffIntroResponse::Continue);
    }
}
