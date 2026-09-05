//! Markdown list detection and parsing; pure over `&str` + byte offset.

use crate::document::EditDelta;

/// A Markdown list found in the source.  All byte offsets; `end` covers the final `\n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListInfo {
    pub start: usize,
    pub end: usize,
    /// Leading whitespace before every item's marker (shared by all items).
    pub indent: String,
    pub kind: MarkerKind,
    pub items: Vec<ListItemInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    /// `-`, `*`, or `+`.
    Bullet(char),
    /// Delimiter `.` or `)`; each item carries its own number.
    Ordered(char),
}

/// One list item.  Byte offsets; `start..end` covers the marker line plus continuation
/// lines and attached blank runs, through the final `\n` when present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItemInfo {
    pub start: usize,
    pub end: usize,
    /// `start + indent.len()`.
    pub marker_start: usize,
    /// Just past `- ` / `1. `, trailing space included.
    pub marker_end: usize,
    /// First byte of user content: `marker_end`, or past the `[ ] ` task prefix.
    pub content_start: usize,
    /// End of the FIRST line (its `\n`, or line end without one).  Deliberately a
    /// first-line fact: marker-adjacent checks only make sense on the marker line.
    pub line_end: usize,
    pub number: Option<u64>,
    /// `None` = not a task; `Some(false)` = `[ ]`; `Some(true)` = `[x]`.
    pub task: Option<bool>,
    /// Byte offset of the checkbox `[` when `task.is_some()`.
    pub task_box: Option<usize>,
}

impl ListItemInfo {
    /// True when the content after the task prefix, continuation lines included, is blank.
    pub fn content_is_empty(&self, source: &str) -> bool {
        let slice = &source[self.content_start..self.end];
        slice.trim().is_empty()
    }
}

/// Delta plus post-edit cursor byte, returned by the `edit` primitives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinueResult {
    pub delta: EditDelta,
    pub cursor_byte: usize,
}

/// Find the list containing byte `cursor_byte` (see the module doc for what a list is).
/// A cursor on a continuation or attached blank line anchors on the nearest marker line
/// above; `None` when the line belongs to no list at its indent.
pub fn find_list_at(source: &str, cursor_byte: usize) -> Option<ListInfo> {
    if source.is_empty() {
        return None;
    }
    let bytes = source.as_bytes();
    let clamped = cursor_byte.min(source.len());
    let cursor_line_start = line_start_byte(bytes, clamped);
    let cursor_line_end = line_end_byte(bytes, cursor_line_start);
    let cursor_line = &source[cursor_line_start..cursor_line_end];

    let anchor_start = if parse_line_start(cursor_line).is_some() {
        cursor_line_start
    } else {
        resolve_anchor_upward(source, bytes, cursor_line_start)?
    };
    let anchor_end = line_end_byte(bytes, anchor_start);
    let (indent, kind, _num) = parse_line_start(&source[anchor_start..anchor_end])?;

    if anchor_start != cursor_line_start
        && !cursor_line.trim().is_empty()
        && !is_continuation_line(cursor_line, &indent)
    {
        return None;
    }

    // Upward: only a marker line commits the extension, so continuation-shaped lines with
    // no marker above are discarded.  A blank directly above a marker line is a
    // list-splitting separator.
    let mut first_start = anchor_start;
    let mut probe = anchor_start;
    let mut below_is_marker = true;
    loop {
        if probe == 0 || bytes[probe - 1] != b'\n' {
            break;
        }
        let ps = line_start_byte(bytes, probe - 1);
        let line = &source[ps..probe - 1];
        if matches_list_line(line, &indent, kind) {
            first_start = ps;
            below_is_marker = true;
        } else if is_continuation_line(line, &indent) {
            below_is_marker = false;
        } else if line.trim().is_empty() {
            if below_is_marker {
                break;
            }
        } else {
            break;
        }
        probe = ps;
    }

    let mut last_end = anchor_end;
    while last_end < source.len() && bytes[last_end] == b'\n' {
        let next_start = last_end + 1;
        if next_start >= source.len() {
            break;
        }
        let next_end = line_end_byte(bytes, next_start);
        let next = &source[next_start..next_end];
        if matches_list_line(next, &indent, kind) || is_continuation_line(next, &indent) {
            last_end = next_end;
        } else if next.trim().is_empty() {
            let Some(resume_end) = blank_run_attaches(source, bytes, next_start, &indent) else {
                break;
            };
            last_end = resume_end;
        } else {
            break;
        }
    }
    if last_end < source.len() && bytes[last_end] == b'\n' {
        last_end += 1;
    }

    // The separator blank below the list is outside it; an edit fired there must not
    // mutate the item above.
    if cursor_line.trim().is_empty() && cursor_line_start >= last_end {
        return None;
    }

    let items = parse_items(source, first_start, last_end, &indent, kind)?;
    if items.is_empty() {
        return None;
    }

    Some(ListInfo {
        start: first_start,
        end: last_end,
        indent,
        kind,
        items,
    })
}

/// Nearest marker line above a non-marker line, crossing only blank and indented lines.
fn resolve_anchor_upward(source: &str, bytes: &[u8], cursor_line_start: usize) -> Option<usize> {
    let mut line_start = cursor_line_start;
    loop {
        if line_start == 0 || bytes[line_start - 1] != b'\n' {
            return None;
        }
        let ps = line_start_byte(bytes, line_start - 1);
        let line = &source[ps..line_start - 1];
        if parse_line_start(line).is_some() {
            return Some(ps);
        }
        if !line.trim().is_empty() && !line.starts_with(' ') && !line.starts_with('\t') {
            return None;
        }
        line_start = ps;
    }
}

/// Non-blank line indented strictly deeper than `list_indent` (lazy continuations at or
/// below the list's indent are deliberately not recognized).
pub(super) fn is_continuation_line(line: &str, list_indent: &str) -> bool {
    let lead_len: usize = line
        .chars()
        .take_while(|&c| c == ' ' || c == '\t')
        .map(char::len_utf8)
        .sum();
    lead_len < line.len() // non-blank
        && lead_len > list_indent.len()
        && line[..lead_len].starts_with(list_indent)
}

/// When the first non-blank line after the blank run at `run_start` is a continuation,
/// return that line's end so the scan resumes past it; `None` for a separator run.
fn blank_run_attaches(
    source: &str,
    bytes: &[u8],
    run_start: usize,
    list_indent: &str,
) -> Option<usize> {
    let mut line_start = run_start;
    loop {
        let line_end = line_end_byte(bytes, line_start);
        let line = &source[line_start..line_end];
        if !line.trim().is_empty() {
            return is_continuation_line(line, list_indent).then_some(line_end);
        }
        if line_end >= source.len() {
            return None;
        }
        line_start = line_end + 1;
    }
}

/// `(indent, kind, number)` for a marker at the start of `line`, or `None`.
pub(super) fn parse_line_start(line: &str) -> Option<(String, MarkerKind, Option<u64>)> {
    let indent_len: usize = line
        .chars()
        .take_while(|&c| c == ' ' || c == '\t')
        .map(char::len_utf8)
        .sum();
    let indent = line[..indent_len].to_owned();
    let rest = &line[indent_len..];
    let mut chars = rest.chars();

    let first = chars.next()?;
    if matches!(first, '-' | '*' | '+') && chars.next() == Some(' ') {
        return Some((indent, MarkerKind::Bullet(first), None));
    }

    let digits_len: usize = rest.chars().take_while(char::is_ascii_digit).count();
    if digits_len > 0 {
        let num: u64 = rest[..digits_len].parse().ok()?;
        let mut after = rest[digits_len..].chars();
        let delim = after.next()?;
        if matches!(delim, '.' | ')') && after.next() == Some(' ') {
            return Some((indent, MarkerKind::Ordered(delim), Some(num)));
        }
    }

    None
}

/// Does `line` start with a marker of the given kind at the given indent?
pub(super) fn matches_list_line(line: &str, indent: &str, kind: MarkerKind) -> bool {
    let Some((line_indent, line_kind, _)) = parse_line_start(line) else {
        return false;
    };
    if line_indent != indent {
        return false;
    }
    match (kind, line_kind) {
        (MarkerKind::Bullet(a), MarkerKind::Bullet(b)) => a == b,
        (MarkerKind::Ordered(a), MarkerKind::Ordered(b)) => a == b,
        _ => false,
    }
}

/// Parse `start..end` (as produced by `find_list_at`'s scans) into items.
pub(super) fn parse_items(
    source: &str,
    start: usize,
    end: usize,
    indent: &str,
    kind: MarkerKind,
) -> Option<Vec<ListItemInfo>> {
    let bytes = source.as_bytes();
    let mut items: Vec<ListItemInfo> = Vec::new();
    let mut cursor = start;
    while cursor < end {
        let item = parse_single_item(source, bytes, cursor, end, indent, kind)?;
        cursor = item.end;
        items.push(item);
    }
    Some(items)
}

/// Parse one item at line start `cursor`; `None` unless it is a marker of this family.
fn parse_single_item(
    source: &str,
    bytes: &[u8],
    cursor: usize,
    end: usize,
    indent: &str,
    kind: MarkerKind,
) -> Option<ListItemInfo> {
    let line_end_pos = line_end_byte(bytes, cursor);
    let line = &source[cursor..line_end_pos];
    let (line_indent, line_kind, number) = parse_line_start(line)?;
    if line_indent != indent {
        return None;
    }
    let marker_start = cursor + line_indent.len();
    let marker_text_len = match line_kind {
        MarkerKind::Bullet(_) => 2,
        MarkerKind::Ordered(_) => {
            let after = &line[line_indent.len()..];
            let digits = after.bytes().take_while(|b| b.is_ascii_digit()).count();
            digits + 2
        }
    };
    let marker_end = marker_start + marker_text_len;
    match (kind, line_kind) {
        (MarkerKind::Bullet(a), MarkerKind::Bullet(b)) if a == b => {}
        (MarkerKind::Ordered(a), MarkerKind::Ordered(b)) if a == b => {}
        _ => return None,
    }

    // `[ ]` without a trailing space is plain content.
    let after_marker = &source[marker_end..line_end_pos];
    let (task, task_box, content_start) = if after_marker.starts_with("[ ] ") {
        (Some(false), Some(marker_end), marker_end + 4)
    } else if after_marker.starts_with("[x] ") || after_marker.starts_with("[X] ") {
        (Some(true), Some(marker_end), marker_end + 4)
    } else {
        (None, None, marker_end)
    };

    // Every following non-marker line in the range is, by the scan's construction, this
    // item's continuation or attached blank.
    let past_line = |content_end: usize| {
        if content_end < end && bytes[content_end] == b'\n' {
            content_end + 1
        } else {
            content_end
        }
    };
    let mut item_end = past_line(line_end_pos);
    while item_end < end {
        let next_end = line_end_byte(bytes, item_end);
        if matches_list_line(&source[item_end..next_end], indent, kind) {
            break;
        }
        item_end = past_line(next_end);
    }
    Some(ListItemInfo {
        start: cursor,
        end: item_end,
        marker_start,
        marker_end,
        content_start,
        line_end: line_end_pos,
        number,
        task,
        task_box,
    })
}

/// Index of the item containing `cursor_byte`; the list's very end counts as the last item.
pub fn cursor_item_idx(info: &ListInfo, cursor_byte: usize) -> Option<usize> {
    for (i, item) in info.items.iter().enumerate() {
        if cursor_byte >= item.start && cursor_byte < item.end {
            return Some(i);
        }
    }
    if cursor_byte == info.end && !info.items.is_empty() {
        return Some(info.items.len() - 1);
    }
    None
}

pub(super) fn line_start_byte(bytes: &[u8], pos: usize) -> usize {
    let mut p = pos.min(bytes.len());
    while p > 0 && bytes[p - 1] != b'\n' {
        p -= 1;
    }
    p
}

pub(super) fn line_end_byte(bytes: &[u8], start: usize) -> usize {
    let mut p = start;
    while p < bytes.len() && bytes[p] != b'\n' {
        p += 1;
    }
    p
}
