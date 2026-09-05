use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::StatefulWidget,
};

use crate::config::Theme;
use crate::editor::vim_ops::VisualKind;
use crate::editor::EditorState;
use crate::ui::line_render::render_raw_line_with_cursor;

/// Raw (plain text) document view: the whole buffer as Markdown source with a block cursor.
pub struct RawView<'a> {
    pub state: &'a EditorState,
    pub theme: &'a Theme,
    /// Active vim Visual flavor.  The half-open `selection` is widened for the highlight only,
    /// via `vim_ops::visual_span`; `selection` itself is never snapped.
    pub visual_kind: Option<VisualKind>,
    /// Block-cursor style for this frame (`app::cursor_style`); follows the vim sub-mode.
    pub cursor_style: Style,
}

#[derive(Debug, Default)]
pub struct RawViewState {
    pub scroll: usize,
}

impl<'a> StatefulWidget for RawView<'a> {
    type State = RawViewState;

    fn render(self, area: Rect, buf: &mut TuiBuf, view_state: &mut Self::State) {
        if area.height == 0 {
            return;
        }

        let height = area.height as usize;
        view_state.scroll = self.state.scroll;

        let width = area.width as usize;
        let (cursor_line, cursor_col) = self.state.cursor.line_col(&self.state.buffer);
        let line_count = self.state.buffer.line_count();
        let cursor_style = self.cursor_style;
        let cursor_visible = self.state.cursor_visible();
        let sel_style = self.theme.selection;
        let selection_range = self.state.selection.map(|s| {
            let r = crate::editor::vim_ops::visual_span(&s, &self.state.buffer, self.visual_kind);
            (r.start, r.end)
        });

        // A live `:s` preview rewrites the buffer, so the search session's byte ranges are
        // stale against it: suspend the search wash until the preview reverts.
        let preview_active = self.state.substitute_preview.is_some();
        let search_matches: &[std::ops::Range<usize>] = if preview_active {
            &[]
        } else {
            self.state
                .search
                .as_ref()
                .map_or(&[], |s| s.matches.as_slice())
        };
        let focused_match = self.state.search.as_ref().map(|s| s.focused_idx);
        let preview_highlights: &[std::ops::Range<usize>] = self
            .state
            .substitute_preview
            .as_ref()
            .map_or(&[], |p| p.highlights.as_slice());
        let rope_len_bytes = self.state.buffer.rope().len_bytes();
        let yank_flash = self.state.active_yank_flash();

        let mut vis_row: usize = 0;
        let (mut buf_line, mut first_sub_row) = self
            .state
            .raw_line_at_visual_row(view_state.scroll, width.max(1));

        while vis_row < height && buf_line < line_count {
            let raw = self.state.buffer.line(buf_line).unwrap_or_default();
            let raw = raw.trim_end_matches('\n');

            let line_char_count = raw.chars().count();
            let line_start_char = self.state.buffer.line_to_char(buf_line);
            let line_end_char = line_start_char + line_char_count;
            let line_sel_cols = selection_range.and_then(|(s, e)| {
                if e <= line_start_char || s > line_end_char {
                    None
                } else {
                    let start = s.saturating_sub(line_start_char);
                    let end = e.saturating_sub(line_start_char).min(line_char_count);
                    if start < end {
                        Some((start, end))
                    } else {
                        None
                    }
                }
            });

            // Highlights as `(start_col, end_col, style)`.  A match, a `:s` span, or a linewise
            // yank flash may cross a line break, so every source is clipped to this line via
            // `push_clipped` rather than skipped.
            let mut line_highlights: Vec<(usize, usize, ratatui::style::Style)> = Vec::new();
            let line_start_byte = self
                .state
                .buffer
                .rope()
                .char_to_byte(line_start_char.min(self.state.buffer.len_chars()));
            let line_end_byte = line_start_byte + raw.len();
            if !search_matches.is_empty() {
                let first = search_matches.partition_point(|m| m.end <= line_start_byte);
                for (i, m) in search_matches.iter().enumerate().skip(first) {
                    if m.start >= line_end_byte {
                        break;
                    }
                    if m.end > rope_len_bytes {
                        break; // stale list (one frame after a content swap)
                    }
                    let style = if Some(i) == focused_match {
                        self.theme.selection
                    } else {
                        self.theme.selection_muted
                    };
                    push_clipped(
                        &mut line_highlights,
                        raw,
                        line_start_byte,
                        line_end_byte,
                        m,
                        style,
                    );
                }
            }

            // `:s` preview spans have no focus concept, so all take the full selection style.
            if !preview_highlights.is_empty() {
                let first = preview_highlights.partition_point(|r| r.end <= line_start_byte);
                for r in preview_highlights.iter().skip(first) {
                    if r.start >= line_end_byte {
                        break;
                    }
                    if r.end > rope_len_bytes {
                        break;
                    }
                    push_clipped(
                        &mut line_highlights,
                        raw,
                        line_start_byte,
                        line_end_byte,
                        r,
                        self.theme.selection,
                    );
                }
            }

            if let Some(flash) = yank_flash {
                push_clipped(
                    &mut line_highlights,
                    raw,
                    line_start_byte,
                    line_end_byte,
                    &(flash.start..flash.end),
                    self.theme.selection,
                );
            }

            // The cursor is painted by the render override, not baked into `display_line`, so
            // the wrap layout matches the scroll / navigation wrap, which never sees the cursor.
            let cursor_override =
                (buf_line == cursor_line && cursor_visible).then_some((cursor_col, cursor_style));
            let display_line = raw_display_line(raw, line_sel_cols, &line_highlights, sel_style);
            // Flat wrap, never a hanging indent: the scroll cache and click mapping wrap at
            // indent 0, and an indent the source lacks would misread as raw text.
            let rows_used = render_raw_line_with_cursor(
                &display_line,
                area,
                buf,
                vis_row as u16,
                cursor_override,
                first_sub_row,
            ) as usize;
            if rows_used == 0 {
                break;
            }

            vis_row += rows_used;
            buf_line += 1;
            first_sub_row = 0;
        }
    }
}

/// Clip an absolute byte `range` to the line `line_start_byte..line_end_byte` (text `raw`,
/// newline excluded) and push any survivor onto `out` as a char-col range.  Every Raw-mode
/// highlight source may cross a line break, so none may assume its range fits the line.
/// `raw.get(..)` rather than indexing keeps a stale post-shrink range from panicking for a frame.
fn push_clipped(
    out: &mut Vec<(usize, usize, ratatui::style::Style)>,
    raw: &str,
    line_start_byte: usize,
    line_end_byte: usize,
    range: &std::ops::Range<usize>,
    style: ratatui::style::Style,
) {
    let start_byte = range.start.max(line_start_byte);
    let end_byte = range.end.min(line_end_byte);
    if start_byte >= end_byte {
        return;
    }
    let s = raw
        .get(..start_byte - line_start_byte)
        .map(|p| p.chars().count());
    let e = raw
        .get(..end_byte - line_start_byte)
        .map(|p| p.chars().count());
    if let (Some(start_col), Some(end_col)) = (s, e) {
        out.push((start_col, end_col, style));
    }
}

/// Styled line for one buffer line: selection background plus highlights, one span per char.
/// The cursor is not painted here (see the render override note in `RawView::render`).
fn raw_display_line(
    raw: &str,
    selection: Option<(usize, usize)>,
    highlights: &[(usize, usize, ratatui::style::Style)],
    selection_style: ratatui::style::Style,
) -> Line<'static> {
    let chars: Vec<char> = raw.chars().collect();
    let mut spans = Vec::with_capacity(chars.len());
    for (i, ch) in chars.iter().enumerate() {
        let in_selection = matches!(selection, Some((s, e)) if i >= s && i < e);
        let highlight = highlights
            .iter()
            .find(|(s, e, _)| i >= *s && i < *e)
            .map(|(_, _, st)| *st);
        let mut style = if in_selection {
            selection_style
        } else {
            ratatui::style::Style::default()
        };
        if let Some(h) = highlight {
            style = style.patch(h);
        }
        spans.push(Span::styled(ch.to_string(), style));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{Buffer, Selection};
    use crate::editor::EditorState;
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    #[test]
    fn raw_view_renders_text() {
        let theme = theme();
        let buf = Buffer::from_str("Hello\nWorld\n");
        let state = EditorState::new(buf, theme);
        let mut view_state = RawViewState::default();

        let backend = TestBackend::new(20, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let view = RawView {
                    visual_kind: None,
                    state: &state,
                    theme,
                    cursor_style: theme.status_mode_raw,
                };
                StatefulWidget::render(view, frame.area(), frame.buffer_mut(), &mut view_state);
            })
            .unwrap();

        let output: String = (0..20u16)
            .map(|x| {
                terminal
                    .backend()
                    .buffer()
                    .cell((x, 0))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect();
        assert!(output.contains('H'), "output: {:?}", output);
    }

    #[test]
    fn raw_view_paints_selection_background() {
        let theme = theme();
        let buf = Buffer::from_str("Hello world\n");
        let mut state = EditorState::new(buf, theme);
        state.selection = Some(Selection {
            anchor: 0,
            active: 5,
        });
        let mut view_state = RawViewState::default();

        let backend = TestBackend::new(20, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let view = RawView {
                    visual_kind: None,
                    state: &state,
                    theme,
                    cursor_style: theme.status_mode_raw,
                };
                StatefulWidget::render(view, frame.area(), frame.buffer_mut(), &mut view_state);
            })
            .unwrap();

        let tbuf = terminal.backend().buffer().clone();
        for x in 0..5u16 {
            let cell = tbuf.cell((x, 0)).expect("cell in bounds");
            assert_eq!(
                cell.style().bg,
                theme.selection.bg,
                "col {} missing selection bg",
                x
            );
        }
        let cell = tbuf.cell((5, 0)).expect("cell in bounds");
        assert_ne!(cell.style().bg, theme.selection.bg);
    }

    #[test]
    fn raw_view_paints_yank_flash() {
        let theme = theme();
        let mut state = EditorState::new(Buffer::from_str("Hello world\n"), theme);
        state.mode = crate::editor::Mode::Raw;
        // Park the cursor off the flashed span so it doesn't recolor col 0.
        state.cursor.offset = state.buffer.len_chars();
        state.flash_yank(0, 5);
        let mut view_state = RawViewState::default();

        let backend = TestBackend::new(20, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let view = RawView {
                    visual_kind: None,
                    state: &state,
                    theme,
                    cursor_style: theme.status_mode_raw,
                };
                StatefulWidget::render(view, frame.area(), frame.buffer_mut(), &mut view_state);
            })
            .unwrap();

        let tbuf = terminal.backend().buffer().clone();
        for x in 0..5u16 {
            assert_eq!(
                tbuf.cell((x, 0)).expect("cell in bounds").style().bg,
                theme.selection.bg,
                "flashed col {x} missing flash bg",
            );
        }
        assert_ne!(
            tbuf.cell((5, 0)).expect("cell in bounds").style().bg,
            theme.selection.bg
        );
    }

    #[test]
    fn raw_view_visual_line_mode_paints_whole_lines() {
        let theme = theme();
        let buf = Buffer::from_str("Hello world\nsecond line\n");
        let mut state = EditorState::new(buf, theme);
        state.mode = crate::editor::Mode::Raw;
        state.selection = Some(Selection {
            anchor: 3,
            active: 15,
        });
        // Park the cursor on the trailing empty line so it doesn't recolor a checked cell.
        state.cursor.offset = state.buffer.len_chars();
        let mut view_state = RawViewState::default();

        let backend = TestBackend::new(20, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let view = RawView {
                    visual_kind: Some(VisualKind::Line),
                    state: &state,
                    theme,
                    cursor_style: theme.status_mode_raw,
                };
                StatefulWidget::render(view, frame.area(), frame.buffer_mut(), &mut view_state);
            })
            .unwrap();

        let tbuf = terminal.backend().buffer().clone();
        // Col 0, which the ragged charwise span (anchor 3) would leave bare, must be painted.
        for x in 0..11u16 {
            assert_eq!(
                tbuf.cell((x, 0)).unwrap().style().bg,
                theme.selection.bg,
                "line 0 col {x} missing selection bg"
            );
            assert_eq!(
                tbuf.cell((x, 1)).unwrap().style().bg,
                theme.selection.bg,
                "line 1 col {x} missing selection bg"
            );
        }
    }

    #[test]
    fn raw_view_visual_scroll_starts_inside_wrapped_line() {
        let theme = theme();
        let buf = Buffer::from_str("abcdefghijklmnopqrstuvwxyz\n");
        let mut state = EditorState::new(buf, theme);
        state.mode = crate::editor::Mode::Raw;
        state.scroll = 1;
        let mut view_state = RawViewState::default();

        let backend = TestBackend::new(10, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let view = RawView {
                    visual_kind: None,
                    state: &state,
                    theme,
                    cursor_style: theme.status_mode_raw,
                };
                StatefulWidget::render(view, frame.area(), frame.buffer_mut(), &mut view_state);
            })
            .unwrap();

        let row: String = (0..10u16)
            .map(|x| {
                terminal
                    .backend()
                    .buffer()
                    .cell((x, 0))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect();
        assert_eq!(row, "klmnopqrst");
    }
}
