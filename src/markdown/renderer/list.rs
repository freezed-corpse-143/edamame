//! `Block::List` rendering.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::markdown::ast::{Block, ListItem};
use crate::markdown::renderer::Renderer;

impl<'t> Renderer<'t> {
    pub(super) fn render_list(
        &self,
        ordered: bool,
        start: Option<u64>,
        items: &[ListItem],
        out: &mut Vec<Line<'static>>,
        indent_prefix: &str,
    ) {
        // `marker_width` is the prefix printed before each item's first line; `nested_indent_width`
        // is the leading whitespace before each nested block.  The latter is `max(4, marker_width)`
        // because source nesting uses 4 spaces, and rendering at the same width keeps the raw-mode
        // view from showing the nested marker shifted; a marker wider than 4 grows the indent with
        // it so those markers still align under their parent's content column.
        let first_num = start.unwrap_or(1);
        let last_num = first_num + items.len().saturating_sub(1) as u64;
        let digit_width = last_num.to_string().len().max(1);
        let marker_width = if ordered { digit_width + 2 } else { 2 };
        let nested_indent_width = marker_width.max(4);
        let child_indent_prefix = format!("{indent_prefix}{}", " ".repeat(nested_indent_width));

        let mut counter = first_num;
        for item in items {
            // Loose-list spacing.  These blanks stay 1:1 with the source lines the reveal maps
            // against, so the count comes straight from `annotate_list_blanks`.
            for _ in 0..item.blank_lines_before {
                out.push(Line::raw(""));
            }
            // A task is a decorated bullet — the same marker plus the checkbox span below — so
            // task items and plain bullets can coexist in one list.
            let (marker, marker_style) = if ordered {
                // Right-aligned in a `digit_width` slot so 10+ doesn't push its text out of
                // alignment with the single-digit items above.
                let s = format!(
                    "{indent_prefix}{counter:>digit_width$}. ",
                    digit_width = digit_width
                );
                counter += 1;
                (s, self.theme.list_number)
            } else {
                let bullet_style = match item.task {
                    Some(true) => self.theme.task_checked,
                    Some(false) => self.theme.task_unchecked,
                    None => self.theme.list_bullet,
                };
                (format!("{indent_prefix}• "), bullet_style)
            };

            let task_prefix: Option<Span<'static>> = item.task.map(|checked| {
                if checked {
                    Span::styled("[✓] ", self.theme.task_checked)
                } else {
                    Span::styled("[ ] ", self.theme.task_unchecked)
                }
            });

            // `task_strikethrough` keeps CROSSED_OUT opt-in, so a theme can mute completed text
            // without striking it through.
            let checked_text_style = if item.task == Some(true) {
                if self.theme.task_strikethrough {
                    self.theme
                        .task_complete_text
                        .add_modifier(ratatui::style::Modifier::CROSSED_OUT)
                } else {
                    self.theme.task_complete_text
                }
            } else {
                Style::default()
            };

            // An empty item still emits its marker so the block produces at least one line.
            if item.blocks.is_empty() {
                let mut spans = vec![Span::styled(marker.clone(), marker_style)];
                if let Some(tp) = task_prefix.clone() {
                    spans.push(tp);
                }
                out.push(Line::from(spans));
                continue;
            }

            for (i, block) in item.blocks.iter().enumerate() {
                if i == 0 {
                    match block {
                        Block::Paragraph { inlines } => {
                            let mut spans = vec![Span::styled(marker.clone(), marker_style)];
                            if let Some(tp) = task_prefix.clone() {
                                spans.push(tp);
                            }
                            spans.extend(self.render_inlines(inlines, checked_text_style));
                            out.push(Line::from(spans));
                        }
                        other => {
                            // A non-paragraph first block gets the marker on a line of its own.
                            let mut spans = vec![Span::styled(marker.clone(), marker_style)];
                            if let Some(tp) = task_prefix.clone() {
                                spans.push(tp);
                            }
                            out.push(Line::from(spans));
                            self.render_block(other, out, &child_indent_prefix, false);
                        }
                    }
                } else {
                    // Later blocks take the child indent, so their text aligns with this item's
                    // text column.
                    self.render_block(block, out, &child_indent_prefix, false);
                }
            }
        }
    }
}
