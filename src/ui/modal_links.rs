//! Inline hyperlinks inside a modal body: how a modal declares one, and the wrap-aware
//! geometry that makes it clickable. See `docs/dev/modal-links.md`.
//!
//! Ratatui exposes no API for where a span landed after wrapping, so a link-bearing modal
//! pre-wraps its body here ([`wrap_rows`], a port of `WordWrapper` at `trim: false`, pinned
//! against ratatui by `wrap_matches_ratatui_paragraph_wrapping`) and hands `Paragraph`
//! rows already cut to width; link geometry falls out of the same pass.
//!
//! Link identity is structural (`(line_idx, span_idx)`), never sniffed from style: the
//! cheat sheet styles *illustrative* link snippets with `theme.link_text`, and
//! `monochrome_dark` spends `UNDERLINED` on headings too.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::docs::DocId;

/// Where a modal link goes: a section of the shipped manual.
///
/// Deliberately not [`crate::editor::link::LinkTarget`]: `Anchor` / `Footnote` would resolve
/// against whatever document sits behind the overlay, and a `LocalFile` naming a manual page
/// is only redirected to the embedded set while a manual page is already open.
///
/// A struct, not an enum: there is exactly one kind of destination. A modal that needs an
/// external `Url` arm should add it with its caller and revisit
/// [`crate::app::App::follow_modal_link`]'s diff-review refusal, which is about replacing the
/// live document — something a browser hand-off does not do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModalLinkTarget {
    /// The page to open.
    pub(crate) id: DocId,
    /// The section within it: a GFM slug, matched exactly — see
    /// [`crate::app::App::heading_line_for_fragment`].
    pub(crate) fragment: Option<&'static str>,
}

/// One hyperlink in a modal's prose body. `line_idx` / `span_idx` index the `body: &[Line]`
/// handed to [`crate::ui::ModalView`]; the whole span is the link, so a link inside a
/// sentence means splitting the sentence into three spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModalLink {
    /// Index into the body's `Line`s.
    pub(crate) line_idx: usize,
    /// Index into that line's `spans`.
    pub(crate) span_idx: usize,
    /// Where following it goes.
    pub(crate) target: ModalLinkTarget,
    /// Visible text, for the flash shown when a link cannot be followed.
    pub(crate) label: String,
}

impl ModalLink {
    /// Declare the span at `(line_idx, span_idx)` as a link to `target`.
    pub(crate) fn new(
        line_idx: usize,
        span_idx: usize,
        target: ModalLinkTarget,
        label: impl Into<String>,
    ) -> Self {
        Self {
            line_idx,
            span_idx,
            target,
            label: label.into(),
        }
    }
}

/// One visual row of a pre-wrapped modal body.
#[derive(Debug, Clone, Default)]
pub(crate) struct WrappedRow {
    /// Already cut to the wrap width; painted with no further reflow.
    pub(crate) line: Line<'static>,
    /// Body coordinate `(line_idx, span_idx)` behind each *display column* (a double-width
    /// grapheme contributes two), so [`link_rects`] needs no re-measuring.
    pub(crate) origins: Vec<(usize, usize)>,
}

/// One grapheme on its way into a row, tagged with where it came from.
struct Piece {
    symbol: String,
    style: Style,
    origin: (usize, usize),
    width: u16,
}

impl Piece {
    fn is_whitespace(&self) -> bool {
        self.symbol.chars().all(char::is_whitespace)
    }
}

/// Wrap `body` to `width` exactly as `Paragraph` with `Wrap { trim: false }` would, keeping
/// each grapheme's origin. A `width` of 0 degrades to one row per body line, matching
/// `wrapped_rows`.
pub(crate) fn wrap_rows(body: &[Line<'_>], width: u16) -> Vec<WrappedRow> {
    if width == 0 {
        return body
            .iter()
            .map(|line| WrappedRow {
                line: owned_line(line),
                origins: Vec::new(),
            })
            .collect();
    }

    let mut out = Vec::new();
    for (line_idx, line) in body.iter().enumerate() {
        wrap_one_line(line, line_idx, width, &mut out);
    }
    // `Paragraph` renders an empty body as nothing; no phantom row here either.
    out
}

/// Mirrors `WordWrapper::process_input` for `trim == false`. Trailing whitespace that would
/// spill past the row edge is dropped.
fn wrap_one_line(line: &Line<'_>, line_idx: usize, width: u16, out: &mut Vec<WrappedRow>) {
    let mut pending_row: Vec<Piece> = Vec::new();
    let mut pending_word: Vec<Piece> = Vec::new();
    let mut pending_ws: std::collections::VecDeque<Piece> = std::collections::VecDeque::new();
    let mut row_width: u16 = 0;
    let mut word_width: u16 = 0;
    let mut ws_width: u16 = 0;
    let mut non_ws_previous = false;
    let start_len = out.len();

    for (span_idx, span) in line.spans.iter().enumerate() {
        for g in span.content.as_ref().graphemes(true) {
            let piece = Piece {
                symbol: g.to_owned(),
                // `Line::styled_graphemes` resolves each grapheme as
                // `line.style.patch(span.style)`; fold the line style in too.
                style: line.style.patch(span.style),
                origin: (line_idx, span_idx),
                width: UnicodeWidthStr::width(g) as u16,
            };
            // Ratatui skips a grapheme too wide to ever fit; without the same skip the loop
            // could never drain it.
            if piece.width > width {
                continue;
            }
            let is_ws = piece.is_whitespace();
            let word_found = non_ws_previous && is_ws;
            let untrimmed_overflow =
                pending_row.is_empty() && word_width + ws_width + piece.width > width;

            if word_found || untrimmed_overflow {
                pending_row.extend(pending_ws.drain(..));
                row_width += ws_width;
                pending_row.append(&mut pending_word);
                row_width += word_width;
                ws_width = 0;
                word_width = 0;
            }

            let row_full = row_width >= width;
            let word_overflow = piece.width > 0 && row_width + ws_width + word_width >= width;
            if row_full || word_overflow {
                let mut remaining = width.saturating_sub(row_width);
                out.push(row_from(std::mem::take(&mut pending_row)));
                row_width = 0;
                // Whitespace that fits before the edge is consumed, so a break does not
                // indent the continuation.
                while let Some(front) = pending_ws.front() {
                    if front.width > remaining {
                        break;
                    }
                    ws_width -= front.width;
                    remaining -= front.width;
                    pending_ws.pop_front();
                }
                if is_ws && pending_ws.is_empty() {
                    continue;
                }
            }

            if is_ws {
                ws_width += piece.width;
                pending_ws.push_back(piece);
            } else {
                word_width += piece.width;
                pending_word.push(piece);
            }
            non_ws_previous = !is_ws;
        }
    }

    pending_row.extend(pending_ws.drain(..));
    pending_row.append(&mut pending_word);
    if !pending_row.is_empty() {
        out.push(row_from(pending_row));
    }
    // An empty line still occupies one row — blank lines are load-bearing spacing.
    if out.len() == start_len {
        out.push(WrappedRow::default());
    }
    // `WordWrapper` carries the line's alignment onto every row, and `Paragraph`'s no-wrap
    // path honors it.
    if let Some(alignment) = line.alignment {
        for row in &mut out[start_len..] {
            row.line = std::mem::take(&mut row.line).alignment(alignment);
        }
    }
}

/// Assemble a row, merging runs that share a style and an origin.
fn row_from(pieces: Vec<Piece>) -> WrappedRow {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut origins: Vec<(usize, usize)> = Vec::new();
    let mut current: Option<(Style, (usize, usize), String)> = None;
    for p in pieces {
        for _ in 0..p.width {
            origins.push(p.origin);
        }
        match &mut current {
            Some((style, origin, text)) if *style == p.style && *origin == p.origin => {
                text.push_str(&p.symbol);
            }
            _ => {
                if let Some((style, _, text)) = current.take() {
                    spans.push(Span::styled(text, style));
                }
                current = Some((p.style, p.origin, p.symbol));
            }
        }
    }
    if let Some((style, _, text)) = current {
        spans.push(Span::styled(text, style));
    }
    WrappedRow {
        line: Line::from(spans),
        origins,
    }
}

/// Owned deep copy keeping the line's own style and alignment, as [`wrap_one_line`] does.
fn owned_line(line: &Line<'_>) -> Line<'static> {
    let mut out = Line::from(
        line.spans
            .iter()
            .map(|s| Span::styled(s.content.as_ref().to_owned(), s.style))
            .collect::<Vec<_>>(),
    )
    .style(line.style);
    out.alignment = line.alignment;
    out
}

/// Column the first cell of `row` is painted at. A centered or right-aligned row is painted
/// shifted, while `origins` is built left-to-right; without this offset the link paints in
/// one place and hit-tests in another. `origins.len()` is the painted width exactly.
fn row_start_col(row: &WrappedRow, width: usize) -> usize {
    use ratatui::layout::Alignment;
    let row_width = row.origins.len();
    let slack = width.saturating_sub(row_width);
    match row.line.alignment {
        Some(Alignment::Center) => slack / 2,
        Some(Alignment::Right) => slack,
        Some(Alignment::Left) | None => 0,
    }
}

/// Absolute terminal rects for every link visible in the scrolled body: one rect per row a
/// link occupies (as [`crate::ui::link_view`] does for the editor). Rows outside the window
/// contribute nothing, which is what makes a scrolled-away link unclickable. Alignment is
/// honored via [`row_start_col`].
pub(crate) fn link_rects(
    rows: &[WrappedRow],
    links: &[ModalLink],
    area: Rect,
    scroll: u16,
) -> Vec<(usize, Rect)> {
    let mut out = Vec::new();
    if area.height == 0 || area.width == 0 {
        return out;
    }
    let first = scroll as usize;
    let last = first.saturating_add(area.height as usize).min(rows.len());
    for (row_idx, row) in rows.iter().enumerate().take(last).skip(first) {
        let y = area.y + (row_idx - first) as u16;
        let offset = row_start_col(row, area.width as usize);
        for (link_idx, link) in links.iter().enumerate() {
            let key = (link.line_idx, link.span_idx);
            let Some(start) = row.origins.iter().position(|o| *o == key) else {
                continue;
            };
            let end = row.origins.iter().rposition(|o| *o == key).unwrap_or(start);
            let start = start + offset;
            let end = end + offset;
            // `area` can be narrower than the wrap width when the body is centered in a
            // wider modal.
            if start >= area.width as usize {
                continue;
            }
            let width = (end + 1 - start).min(area.width as usize - start);
            out.push((
                link_idx,
                Rect {
                    x: area.x + start as u16,
                    y,
                    width: width as u16,
                    height: 1,
                },
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::{Paragraph, Widget, Wrap};
    use ratatui::Terminal;

    fn body() -> Vec<Line<'static>> {
        vec![
            Line::raw("The quick brown fox jumps over the lazy dog and keeps running onward"),
            Line::raw(""),
            Line::raw("  indented continuation text that is long enough to wrap at least once"),
            Line::raw("supercalifragilisticexpialidociousandthensomemoretoforceahardsplit"),
            Line::from(vec![
                Span::raw("See "),
                Span::raw("Terminal compatibility"),
                Span::raw(" for the list of terminals."),
            ]),
            // Alignment moves symbols, so the cell diff checks the carried-over alignment too.
            Line::raw("a centred paragraph that has to wrap somewhere")
                .alignment(ratatui::layout::Alignment::Center),
        ]
    }

    /// Render `lines` through ratatui's `Wrap { trim: false }` and through our pre-wrap.
    fn both_renderings(
        lines: &[Line<'static>],
        width: u16,
        height: u16,
    ) -> (Vec<String>, Vec<String>) {
        let cells = |f: &dyn Fn(Rect, &mut ratatui::buffer::Buffer)| -> Vec<String> {
            let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
            term.draw(|frame| {
                let area = Rect::new(0, 0, width, height);
                f(area, frame.buffer_mut());
            })
            .unwrap();
            let buf = term.backend().buffer().clone();
            (0..height)
                .map(|y| {
                    (0..width)
                        .map(|x| buf[(x, y)].symbol().to_owned())
                        .collect::<String>()
                })
                .collect()
        };
        let native = cells(&|area, buf| {
            Paragraph::new(lines.to_vec())
                .wrap(Wrap { trim: false })
                .render(area, buf);
        });
        let ours = cells(&|area, buf| {
            let rows = wrap_rows(lines, area.width);
            let painted: Vec<Line<'static>> = rows.into_iter().map(|r| r.line).collect();
            Paragraph::new(painted).render(area, buf);
        });
        (native, ours)
    }

    #[test]
    fn wrap_matches_ratatui_paragraph_wrapping() {
        // Widths chosen to exercise mid-word splits, whitespace-at-the-edge, and hard splits.
        for width in [12u16, 17, 20, 31, 40, 79] {
            let (native, ours) = both_renderings(&body(), width, 30);
            assert_eq!(native, ours, "wrap diverged at width {width}");
        }
    }

    /// Declaring a link must not strip a line-level style.
    #[test]
    fn a_line_level_style_survives_the_pre_wrap() {
        use ratatui::style::{Color, Modifier};
        let base = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
        let lines = vec![Line::from(vec![
            Span::raw("plain "),
            Span::styled("green", Style::default().fg(Color::Green)),
        ])
        .style(base)];

        let rows = wrap_rows(&lines, 40);
        let spans = &rows[0].line.spans;
        assert_eq!(spans[0].style.fg, Some(Color::Red), "the line's own color");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        // A span's own color still wins; the line supplies the rest.
        assert_eq!(spans[1].style.fg, Some(Color::Green));
        assert!(
            spans[1].style.add_modifier.contains(Modifier::BOLD),
            "the line's modifier reaches a span that sets only a color"
        );
    }

    /// Same for alignment.
    #[test]
    fn a_line_alignment_survives_the_pre_wrap() {
        use ratatui::layout::Alignment;
        let lines = vec![
            Line::raw("alpha bravo charlie delta echo").alignment(Alignment::Center),
            Line::raw("left"),
        ];
        let rows = wrap_rows(&lines, 12);
        assert!(rows.len() > 2, "the first line wrapped");
        for row in &rows[..rows.len() - 1] {
            assert_eq!(
                row.line.alignment,
                Some(Alignment::Center),
                "every row a centred line wrapped into stays centred"
            );
        }
        assert_eq!(rows.last().expect("a row").line.alignment, None);
    }

    #[test]
    fn a_blank_body_line_still_occupies_one_row() {
        let rows = wrap_rows(&[Line::raw("a"), Line::raw(""), Line::raw("b")], 10);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].line.width(), 0);
    }

    #[test]
    fn origins_name_the_span_behind_every_column() {
        let lines = vec![Line::from(vec![Span::raw("ab"), Span::raw("cd")])];
        let rows = wrap_rows(&lines, 10);
        assert_eq!(
            rows[0].origins,
            vec![(0, 0), (0, 0), (0, 1), (0, 1)],
            "each column reports the span it came from"
        );
    }

    #[test]
    fn a_double_width_grapheme_claims_two_columns() {
        let lines = vec![Line::from(vec![Span::raw("東"), Span::raw("x")])];
        let rows = wrap_rows(&lines, 10);
        assert_eq!(rows[0].origins, vec![(0, 0), (0, 0), (0, 1)]);
    }

    #[test]
    fn a_link_rect_covers_exactly_its_span() {
        let lines = vec![Line::from(vec![
            Span::raw("See "),
            Span::raw("the docs"),
            Span::raw(" now"),
        ])];
        let rows = wrap_rows(&lines, 40);
        let links = vec![ModalLink::new(
            0,
            1,
            ModalLinkTarget {
                id: DocId::Index,
                fragment: None,
            },
            "the docs",
        )];
        let rects = link_rects(&rows, &links, Rect::new(3, 5, 40, 4), 0);
        assert_eq!(rects.len(), 1);
        let (idx, r) = rects[0];
        assert_eq!(idx, 0);
        assert_eq!((r.x, r.y, r.width, r.height), (3 + 4, 5, 8, 1));
    }

    #[test]
    fn a_wrapped_link_reports_one_rect_per_row() {
        let lines = vec![Line::from(vec![
            Span::raw("x "),
            Span::raw("alpha beta gamma"),
        ])];
        let rows = wrap_rows(&lines, 12);
        let links = vec![ModalLink::new(
            0,
            1,
            ModalLinkTarget {
                id: DocId::Index,
                fragment: None,
            },
            "alpha beta gamma",
        )];
        let rects = link_rects(&rows, &links, Rect::new(0, 0, 12, 6), 0);
        assert!(
            rects.len() >= 2,
            "a link wrapping across rows hit-tests on each: {rects:?}"
        );
        assert!(rects.iter().all(|(i, _)| *i == 0));
    }

    #[test]
    fn a_scrolled_away_link_yields_no_rect() {
        let lines = vec![
            Line::raw("one"),
            Line::raw("two"),
            Line::from(vec![Span::raw("three")]),
        ];
        let rows = wrap_rows(&lines, 20);
        let links = vec![ModalLink::new(
            2,
            0,
            ModalLinkTarget {
                id: DocId::Index,
                fragment: None,
            },
            "three",
        )];
        let rects = link_rects(&rows, &links, Rect::new(0, 0, 20, 1), 0);
        assert!(rects.is_empty());
        let rects = link_rects(&rows, &links, Rect::new(0, 0, 20, 1), 2);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].1.y, 0, "the row paints at the top of the window");
    }

    // ── Alignment ─────────────────────────────────────────────────

    /// Paint `lines` the way `ModalView`'s link path does and read back the cells each
    /// reported rect covers — geometry checked against what ratatui painted, not against a
    /// second derivation of the same arithmetic.
    fn painted_link_text(lines: &[Line<'static>], links: &[ModalLink], width: u16) -> Vec<String> {
        let height = 6u16;
        let area = Rect::new(0, 0, width, height);
        let rows = wrap_rows(lines, width);
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|frame| {
            let painted: Vec<Line<'static>> = rows.iter().map(|r| r.line.clone()).collect();
            Paragraph::new(painted).render(area, frame.buffer_mut());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        link_rects(&rows, links, area, 0)
            .into_iter()
            .map(|(_, r)| {
                (r.x..r.x + r.width)
                    .map(|x| buf[(x, r.y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect()
    }

    /// Reading straight off the left-to-right `origins` would put a centered link's rect
    /// half the slack to the left of its text.
    #[test]
    fn a_centred_link_hit_tests_where_it_is_painted() {
        let lines = vec![Line::from(vec![
            Span::raw("See "),
            Span::raw("the docs"),
            Span::raw(" now"),
        ])
        .alignment(ratatui::layout::Alignment::Center)];
        let links = vec![ModalLink::new(
            0,
            1,
            ModalLinkTarget {
                id: DocId::Index,
                fragment: None,
            },
            "the docs",
        )];
        assert_eq!(painted_link_text(&lines, &links, 40), vec!["the docs"]);
    }

    #[test]
    fn a_right_aligned_link_hit_tests_where_it_is_painted() {
        let lines = vec![Line::from(vec![Span::raw("See "), Span::raw("the docs")])
            .alignment(ratatui::layout::Alignment::Right)];
        let links = vec![ModalLink::new(
            0,
            1,
            ModalLinkTarget {
                id: DocId::Index,
                fragment: None,
            },
            "the docs",
        )];
        assert_eq!(painted_link_text(&lines, &links, 40), vec!["the docs"]);
    }

    /// The offset is zero for Left and `None`.
    #[test]
    fn an_unaligned_link_is_unmoved_by_the_alignment_offset() {
        let lines = vec![Line::from(vec![
            Span::raw("See "),
            Span::raw("the docs"),
            Span::raw(" now"),
        ])];
        let links = vec![ModalLink::new(
            0,
            1,
            ModalLinkTarget {
                id: DocId::Index,
                fragment: None,
            },
            "the docs",
        )];
        assert_eq!(painted_link_text(&lines, &links, 40), vec!["the docs"]);
    }
}
