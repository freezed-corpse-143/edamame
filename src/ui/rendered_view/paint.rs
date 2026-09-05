use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::config::Theme;
use crate::editor::table_edit;
use crate::editor::EditorState;
use crate::markdown::table_layout::CellOverlay;

use super::raw_text::{raw_line_byte_start, raw_source_lines};
use crate::markdown::code_layout::{code_raw_col_to_rendered_col, is_code_fence_row};
use crate::markdown::list_layout::{raw_list_marker_char_width, rendered_list_marker_char_width};

/// [`make_raw_line_with_selection`] with no selection.
#[cfg(test)]
pub(super) fn make_raw_line(raw_text: &str, theme: &Theme) -> Line<'static> {
    make_raw_line_with_selection(raw_text, None, theme)
}

/// A `Line` of `raw_text` (the raw-revealed cursor block) with `selection_cols` (a `[start, end)`
/// char range) painted in the selection background.
///
/// The cursor is NOT embedded: `line_render` paints it onto the resolved cell, so the wrap is
/// computed from the bare source and matches the wrap the scroll/navigation code uses.
pub(super) fn make_raw_line_with_selection(
    raw_text: &str,
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
) -> Line<'static> {
    make_raw_line_over(raw_text, selection_cols, theme, theme.normal)
}

/// [`make_raw_line_with_selection`] over a caller-supplied `base` style, so a revealed line
/// inside a block with its own surface (a blockquote wash) keeps that surface. `base` is also
/// the line-level style, so `line_render`'s trailing-cell fill carries it to the viewport edge.
pub(super) fn make_raw_line_over(
    raw_text: &str,
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
    base: Style,
) -> Line<'static> {
    let sel_style = theme.selection;
    let chars: Vec<char> = raw_text.chars().collect();
    let total = chars.len();

    // One span per char keeps per-char styling predictable when cursor and selection overlap.
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(total);
    for (i, ch) in chars.iter().enumerate() {
        let mut style = base;
        if matches!(selection_cols, Some((s, e)) if i >= s && i < e) {
            style = style.patch(sel_style);
        }
        spans.push(Span::styled(ch.to_string(), style));
    }
    Line::from(spans).style(base)
}

/// One body row of a mermaid block revealed as a code block, with `code_block_text` as the
/// line style so the trailing-cell fill extends the code background. Char positions stay 1:1
/// with `raw_text` (no leading pad) so click → raw col mapping needs no adjustment.
pub(super) fn make_code_styled_body_line(
    raw_text: &str,
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
) -> Line<'static> {
    let base = theme.code_block_text;
    let sel_style = theme.selection;
    let chars: Vec<char> = raw_text.chars().collect();
    let total = chars.len();

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(total);
    for (i, ch) in chars.iter().enumerate() {
        let mut style = base;
        if matches!(selection_cols, Some((s, e)) if i >= s && i < e) {
            style = style.patch(sel_style);
        }
        spans.push(Span::styled(ch.to_string(), style));
    }
    Line::from(spans).style(base)
}

/// Post-render pass: paint `style` over the rendered cells of one rendered line for the source
/// byte range `[sel_start_byte, sel_end_byte)`. Shared by the selection, search-match, `:s`
/// preview, and yank-flash overlays.
///
/// Intersects the range with *this rendered line's* raw bytes within its block, then maps the
/// covered raw cols to rendered cols per block kind. Keep the per-line clamp: multi-line
/// ranges (a search match containing `\n`, a linewise selection) rely on it.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_byte_range_overlay(
    editor: &EditorState,
    buf: &mut TuiBuf,
    area: Rect,
    y_start: u16,
    rows_used: u16,
    skip_rows: usize,
    rendered_line_idx: usize,
    sel_start_byte: usize,
    sel_end_byte: usize,
    style: Style,
) {
    let Some(block_byte) = editor
        .parsed
        .source_map
        .original_byte_for_rendered_line(rendered_line_idx)
    else {
        return;
    };
    let Some(block_range) = editor.parsed.source_map.original_range_for_byte(block_byte) else {
        return;
    };
    if block_range.end <= sel_start_byte || block_range.start >= sel_end_byte {
        return;
    }

    let source = editor.buffer.contents();
    // `get` rather than indexing: with `parsed_dirty` set, an in-line edit may have shifted
    // offsets so `block_range` ends inside a multi-byte sequence. Skipping one frame is fine.
    let block_text = source
        .get(block_range.start..block_range.end.min(source.len()))
        .unwrap_or("");
    let rendered_span = editor
        .parsed
        .source_map
        .rendered_lines_for_byte(block_range.start);
    let sub_idx_in_block = rendered_line_idx.saturating_sub(rendered_span.start);
    let is_table = table_edit::is_table_block(block_text);
    // Wrap-chunk index of a table sub-line within its logical row.
    let mut table_sub = 0usize;
    let raw_line_idx = if is_table {
        // Rows can wrap, so classify by box-drawing glyph rather than assume alternation.
        let own_end = rendered_span.end.min(editor.parsed.lines.len());
        let block_lines = editor
            .parsed
            .lines
            .get(rendered_span.start..own_end)
            .unwrap_or(&[]);
        let kinds = crate::ui::table_view::classify_table_sub_lines(block_lines);
        match kinds.get(sub_idx_in_block) {
            Some(crate::ui::table_view::TableSubLineKind::Header { sub }) => {
                table_sub = *sub;
                0
            }
            Some(crate::ui::table_view::TableSubLineKind::DataRow { row, sub }) => {
                table_sub = *sub;
                row + 2
            }
            // Separators and borders carry no raw-byte mapping.
            _ => return,
        }
    } else {
        sub_idx_in_block
    };

    let raw_lines: Vec<&str> = block_text.split('\n').collect();
    // Real lines (no phantom trailing entry) — the fence test below must use these.
    let content_lines = raw_source_lines(block_text);
    if raw_line_idx >= raw_lines.len() {
        return;
    }
    let raw_line = raw_lines[raw_line_idx];
    let raw_line_start = raw_line_byte_start(block_text, raw_line_idx);
    let raw_line_start_abs = block_range.start + raw_line_start;
    let raw_line_end_abs = raw_line_start_abs + raw_line.len();

    let line_sel_start = sel_start_byte.max(raw_line_start_abs);
    let line_sel_end = sel_end_byte.min(raw_line_end_abs);
    if line_sel_start >= line_sel_end {
        return;
    }

    let start_raw_col = raw_line[..line_sel_start - raw_line_start_abs]
        .chars()
        .count();
    let end_raw_col = raw_line[..line_sel_end - raw_line_start_abs]
        .chars()
        .count();

    let Some(line) = editor.parsed.lines.get(rendered_line_idx) else {
        return;
    };

    let buffer_line_idx = editor
        .buffer
        .block_line_to_buffer_line(block_range.start, raw_line_idx);
    let inline_map = editor.inline_map_for(buffer_line_idx, raw_line);
    let actual_rendered: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();

    // Headings (level-deep space prefix) and code blocks (one leading pad cell) shift the
    // rendered text in ways the inline collapse map doesn't model. Looked up via
    // `real_block_for_byte` — a source-map block index must NOT index `parsed.blocks`, whose
    // index space counts blank-line virtual blocks.
    let block_kind = editor.parsed.real_block_for_byte(line_sel_start);

    // `Some` only for a real `Block::List` whose raw line carries its own marker.
    let list_marker_widths = if matches!(block_kind, Some(crate::markdown::Block::List { .. })) {
        raw_list_marker_char_width(raw_line).zip(rendered_list_marker_char_width(line))
    } else {
        None
    };

    if is_table {
        // A match's raw cols land in at most one wrap chunk per cell; mapping per `table_sub`
        // keeps the highlight off sub-lines that don't show the matched text.
        for (rs, re) in crate::markdown::table_layout::table_raw_col_range_to_rendered_segments(
            raw_line,
            line,
            start_raw_col,
            end_raw_col,
            table_sub,
        ) {
            paint_cols_on_line(
                line, buf, area, y_start, rows_used, skip_rows, rs, re, style,
            );
        }
        return;
    }

    let (rend_start, rend_end) =
        if let Some(crate::markdown::Block::CodeBlock { fenced, .. }) = block_kind {
            // Body rows render 1:1 behind a pad cell (indented blocks also drop the stripped
            // indent); both terms live in `markdown::code_layout`. Fence rows render unrelated
            // text, so wash the whole row instead of mapping columns. Fence detection uses
            // `content_lines`, not `raw_lines`: the phantom trailing entry would make the
            // closing fence look like a body row.
            if is_code_fence_row(*fenced, raw_line_idx, &content_lines) {
                (0, actual_rendered)
            } else {
                let map_col = |c: usize| code_raw_col_to_rendered_col(raw_line, *fenced, c);
                (map_col(start_raw_col), map_col(end_raw_col))
            }
        } else if matches!(
            block_kind,
            Some(crate::markdown::Block::MetadataBlock { .. })
        ) {
            // Frontmatter renders verbatim, so the mapping is the identity. The inline map
            // must not be consulted: it re-parses the line as Markdown (smart quotes, `*`
            // emphasis) that the rendered row doesn't have.
            (
                start_raw_col.min(actual_rendered),
                end_raw_col.min(actual_rendered),
            )
        } else if let Some(crate::markdown::Block::Heading { level, .. }) = block_kind {
            // Prefix shift plus the collapse map. A length mismatch (big-H1 rows, setext
            // underline) skips the highlight rather than painting one off-by-prefix.
            let prefix = heading_prefix_width(*level);
            let content_rendered = actual_rendered.saturating_sub(prefix);
            match (
                inline_map.raw_to_rendered_checked(start_raw_col, content_rendered),
                inline_map.raw_to_rendered_checked(end_raw_col, content_rendered),
            ) {
                (Some(rs), Some(re)) => (rs + prefix, re + prefix),
                _ => return,
            }
        } else if let Some((rmw, rmw_r)) = list_marker_widths {
            // Marker widths handle the `- ` / `1. ` shift, composed with the collapse map.
            // Gated on the AST kind: a Paragraph that merely sniffs like a marker, or a
            // continuation line without its own marker, takes the paragraph mapping.
            let content_rendered = actual_rendered.saturating_sub(rmw_r);
            let map_col = |raw_col: usize| -> Option<usize> {
                if raw_col < rmw {
                    Some(rmw_r)
                } else {
                    inline_map
                        .raw_to_rendered_checked(raw_col, content_rendered)
                        .map(|c| c + rmw_r)
                }
            };
            match (map_col(start_raw_col), map_col(end_raw_col)) {
                (Some(mut rend_start), Some(rend_end)) => {
                    // A selection reaching col 0 (VisualLine, or a fully covered intermediate
                    // line) must paint the marker too; the map snapped the start forward.
                    if start_raw_col == 0 {
                        rend_start = 0;
                    }
                    (rend_start, rend_end)
                }
                // Mismatch: skip rather than paint off-by-N, except a col-0 start still
                // washes the whole row so line selections never vanish.
                _ if start_raw_col == 0 => (0, actual_rendered),
                _ => return,
            }
        } else {
            match (
                inline_map.raw_to_rendered_checked(start_raw_col, actual_rendered),
                inline_map.raw_to_rendered_checked(end_raw_col, actual_rendered),
            ) {
                (Some(rs), Some(re)) => (rs, re),
                _ => (start_raw_col, end_raw_col),
            }
        };
    if rend_start >= rend_end {
        return;
    }
    paint_cols_on_line(
        line, buf, area, y_start, rows_used, skip_rows, rend_start, rend_end, style,
    );
}

/// Cells of the space prefix before a heading's content (one per level — see
/// `Renderer::render_heading`).
fn heading_prefix_width(level: pulldown_cmark::HeadingLevel) -> usize {
    use pulldown_cmark::HeadingLevel::*;
    match level {
        H1 => 1,
        H2 => 2,
        H3 => 3,
        H4 => 4,
        H5 => 5,
        H6 => 6,
    }
}

/// Post-render pass: paint every visible search match; the focused one in `theme.selection`,
/// the rest in `selection_muted`. Called by `EditorView` for both Preview and Rendered, which
/// share the same wrap. Ranges are clamped against the live source so a stale match list
/// (one frame after an external content swap) skips rather than panics.
pub(crate) fn paint_search_overlays(
    editor: &EditorState,
    buf: &mut TuiBuf,
    area: Rect,
    theme: &Theme,
) {
    let Some(search) = editor.search.as_ref() else {
        return;
    };
    // A live `:s` preview rewrites the buffer, so search byte ranges are stale against it.
    if editor.substitute_preview.is_some() {
        return;
    }
    if search.matches.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let source_len = editor.buffer.rope().len_bytes();
    let width = area.width as usize;
    let (mut line_idx, mut first_sub_row) =
        editor.rendered_line_at_visual_row(editor.scroll, width.max(1));
    let mut vis_y: u16 = 0;
    while vis_y < area.height {
        if line_idx >= editor.parsed.lines.len() {
            break;
        }
        let rows = editor
            .parsed
            .visual_rows_for_line_at(line_idx, width)
            .max(1);
        let painted = rows
            .saturating_sub(first_sub_row)
            .min((area.height - vis_y) as usize);
        if painted == 0 {
            break;
        }
        let block_range = editor
            .parsed
            .source_map
            .original_byte_for_rendered_line(line_idx)
            .and_then(|b| editor.parsed.source_map.original_range_for_byte(b));
        if let Some(block_range) = block_range {
            // Matches are sorted: jump to the first that could touch this block.
            let start = search
                .matches
                .partition_point(|m| m.end <= block_range.start);
            for (i, m) in search.matches.iter().enumerate().skip(start) {
                if m.start >= block_range.end {
                    break;
                }
                if m.end > source_len {
                    continue;
                }
                let style = if i == search.focused_idx {
                    theme.selection
                } else {
                    theme.selection_muted
                };
                paint_byte_range_overlay(
                    editor,
                    buf,
                    area,
                    vis_y,
                    painted as u16,
                    first_sub_row,
                    line_idx,
                    m.start,
                    m.end,
                    style,
                );
            }
        }
        vis_y += painted as u16;
        line_idx += 1;
        first_sub_row = 0;
    }
}

/// Post-render pass: paint the live `:s` preview's highlight ranges (matches while typing the
/// pattern, inserted replacement segments once one exists). Same walk as
/// [`paint_search_overlays`], single style — the preview has no focus concept.
pub(crate) fn paint_substitute_preview_overlays(
    editor: &EditorState,
    buf: &mut TuiBuf,
    area: Rect,
    theme: &Theme,
) {
    let Some(preview) = editor.substitute_preview.as_ref() else {
        return;
    };
    if preview.highlights.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let source_len = editor.buffer.rope().len_bytes();
    let width = area.width as usize;
    let (mut line_idx, mut first_sub_row) =
        editor.rendered_line_at_visual_row(editor.scroll, width.max(1));
    let mut vis_y: u16 = 0;
    while vis_y < area.height {
        if line_idx >= editor.parsed.lines.len() {
            break;
        }
        let rows = editor
            .parsed
            .visual_rows_for_line_at(line_idx, width)
            .max(1);
        let painted = rows
            .saturating_sub(first_sub_row)
            .min((area.height - vis_y) as usize);
        if painted == 0 {
            break;
        }
        let block_range = editor
            .parsed
            .source_map
            .original_byte_for_rendered_line(line_idx)
            .and_then(|b| editor.parsed.source_map.original_range_for_byte(b));
        if let Some(block_range) = block_range {
            let start = preview
                .highlights
                .partition_point(|r| r.end <= block_range.start);
            for r in preview.highlights.iter().skip(start) {
                if r.start >= block_range.end {
                    break;
                }
                if r.end > source_len {
                    continue;
                }
                paint_byte_range_overlay(
                    editor,
                    buf,
                    area,
                    vis_y,
                    painted as u16,
                    first_sub_row,
                    line_idx,
                    r.start,
                    r.end,
                    theme.selection,
                );
            }
        }
        vis_y += painted as u16;
        line_idx += 1;
        first_sub_row = 0;
    }
}

/// Post-render pass: neovim-style yank highlight over [`EditorState::yank_flash`]'s range.
/// Same walk as [`paint_search_overlays`].
pub(crate) fn paint_yank_flash(editor: &EditorState, buf: &mut TuiBuf, area: Rect, theme: &Theme) {
    let Some(flash) = editor.active_yank_flash() else {
        return;
    };
    if area.width == 0 || area.height == 0 {
        return;
    }
    let source_len = editor.buffer.rope().len_bytes();
    if flash.start >= flash.end || flash.end > source_len {
        return;
    }
    let width = area.width as usize;
    let (mut line_idx, mut first_sub_row) =
        editor.rendered_line_at_visual_row(editor.scroll, width.max(1));
    let mut vis_y: u16 = 0;
    while vis_y < area.height {
        if line_idx >= editor.parsed.lines.len() {
            break;
        }
        let rows = editor
            .parsed
            .visual_rows_for_line_at(line_idx, width)
            .max(1);
        let painted = rows
            .saturating_sub(first_sub_row)
            .min((area.height - vis_y) as usize);
        if painted == 0 {
            break;
        }
        let block_range = editor
            .parsed
            .source_map
            .original_byte_for_rendered_line(line_idx)
            .and_then(|b| editor.parsed.source_map.original_range_for_byte(b));
        if let Some(block_range) = block_range {
            if block_range.start < flash.end && block_range.end > flash.start {
                paint_byte_range_overlay(
                    editor,
                    buf,
                    area,
                    vis_y,
                    painted as u16,
                    first_sub_row,
                    line_idx,
                    flash.start,
                    flash.end,
                    theme.selection,
                );
            }
        }
        vis_y += painted as u16;
        line_idx += 1;
        first_sub_row = 0;
    }
}

/// Paint `sel_bg` onto the rendered cells for rendered char cols in
/// `[start_col, end_col)`, walking each visual row of the wrapped line.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_cols_on_line(
    line: &Line<'_>,
    buf: &mut TuiBuf,
    area: Rect,
    y_start: u16,
    rows_used: u16,
    skip_rows: usize,
    start_col: usize,
    end_col: usize,
    sel_bg: Style,
) {
    let width = area.width as usize;
    if width == 0 || end_col <= start_col {
        return;
    }
    let chars: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| {
            let style = span.style;
            span.content.chars().map(move |c| (c, style))
        })
        .collect();
    let indent = crate::ui::line_render::compute_hanging_indent(line);
    let rows = crate::ui::line_render::visual_rows_of_chars(&chars, width, indent);
    for (painted_off, (row_off, &(row_start, row_end, _))) in
        rows.iter().enumerate().skip(skip_rows).enumerate()
    {
        if painted_off as u16 >= rows_used {
            break;
        }
        let y = area.y + y_start + painted_off as u16;
        if y >= area.y + area.height {
            break;
        }
        let row_sel_start = start_col.max(row_start);
        let row_sel_end = end_col.min(row_end);
        if row_sel_start >= row_sel_end {
            continue;
        }
        // Continuation rows are pre-padded with `indent` cells; shift by the same amount.
        let row_indent = if row_off == 0 { 0 } else { indent };
        for i in row_sel_start..row_sel_end {
            let x_off = row_indent + (i - row_start);
            let x = area.x + x_off as u16;
            if x >= area.x + area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(cell.style().patch(sel_bg));
            }
        }
    }
}

/// Paint `overlay.raw_text` into the cell's rendered column range, directly into the buffer
/// (the underlying row must already be rendered). `selection_cols` is painted here because
/// the overlay clobbers whatever the generic selection pass already painted.
pub(super) fn overlay_raw_cell(
    buf: &mut TuiBuf,
    area: Rect,
    visual_y: u16,
    overlay: &CellOverlay,
    selection_cols: Option<(usize, usize)>,
    theme: &Theme,
    // Block-cursor style when visible this frame; `None` when blinked off or not in this row.
    cursor: Option<Style>,
) {
    if visual_y >= area.height {
        return;
    }
    let abs_y = area.y + visual_y;
    let cell_width = overlay.rendered_end.saturating_sub(overlay.rendered_start);
    let raw_chars: Vec<char> = overlay.raw_text.chars().collect();
    // Strip `theme.normal`'s bg so the table-row stripe under the cell survives.
    let base_style = Style {
        bg: None,
        ..theme.normal
    };

    for i in 0..cell_width {
        let col = overlay.rendered_start + i;
        let abs_x = area.x.saturating_add(col as u16);
        if abs_x >= area.x.saturating_add(area.width) {
            break;
        }
        let ch = raw_chars.get(i).copied().unwrap_or(' ');
        let mut style = base_style;
        if matches!(selection_cols, Some((s, e)) if i >= s && i < e) {
            style = style.patch(theme.selection);
        }
        if let Some(cursor_style) = cursor.filter(|_| overlay.cursor_in_cell == Some(i)) {
            style = cursor_style;
        }
        if let Some(cell) = buf.cell_mut((abs_x, abs_y)) {
            // `Cell::set_style` only adds/removes modifiers, so e.g. the BOLD of a rendered
            // `**x**` would bleed through the raw chars; zero them by hand.
            cell.modifier = Modifier::empty();
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::Theme;

    #[test]
    fn make_raw_line_keeps_source_text_verbatim() {
        // The cursor is painted onto the resolved cell, not baked into the line.
        let theme = Theme::default();
        let line = make_raw_line("hello", &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hello");
    }

    #[test]
    fn make_raw_line_with_selection_paints_range() {
        let theme = Theme::default();
        let line = make_raw_line_with_selection("hello", Some((1, 3)), &theme);
        assert_eq!(line.spans[1].style.bg, theme.selection.bg);
        assert_eq!(line.spans[2].style.bg, theme.selection.bg);
        assert_ne!(line.spans[0].style.bg, theme.selection.bg);
    }
}
