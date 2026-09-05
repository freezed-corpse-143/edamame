//! AST post-passes that run after `parse_raw`, reshaping pulldown-cmark's block list for
//! the renderer: image paragraphs and mermaid code blocks become `Block::ImageBlock`,
//! pure-comment HTML becomes `Block::HtmlComment`, trailing `<!-- tui-columns -->`
//! comments fold into preceding tables, and loose list items record the blank source lines
//! preceding them.

use std::collections::HashMap;
use std::ops::Range;

use crate::diagram::DiagramSource;
use crate::markdown::ast::{Block, Inline};

/// Collapse a `Block::Paragraph` whose only substantive inline is an `Inline::Image` into
/// a `Block::ImageBlock`.  `real_ranges` is untouched — the promotion removes no blocks;
/// the parameter exists for symmetry with [`attach_trailing_tui_columns_comments`].
pub fn promote_image_paragraphs(
    blocks: &mut [Block],
    _real_ranges: Option<&mut Vec<Range<usize>>>,
) {
    for block in blocks.iter_mut() {
        if let Block::Paragraph { inlines } = block {
            if let Some((alt, url)) = extract_lone_image(inlines) {
                *block = Block::ImageBlock { alt, url };
            }
        }
    }
}

/// Replace every `mermaid`-tagged fenced code block with a synthetic `Block::ImageBlock`,
/// returning the `url → DiagramSource` map so `ParsedDoc` can hand the source to the
/// decode worker.
///
/// Only the bare `mermaid` tag is matched (not `mermaidjs`, `diagram`, …): GitHub and
/// mermaid.js accept only that, so accepting more would render here what falls back to a
/// code block everywhere else.  Called only from
/// [`crate::document::ParsedDoc::build_with_overrides`], so `parse`'s other consumers
/// still see the raw code block.
pub fn promote_diagram_code_blocks(blocks: &mut [Block]) -> HashMap<String, DiagramSource> {
    let mut sources = HashMap::new();
    for block in blocks.iter_mut() {
        let is_mermaid = matches!(
            block,
            Block::CodeBlock { language: Some(lang), .. } if lang.eq_ignore_ascii_case("mermaid")
        );
        if !is_mermaid {
            continue;
        }
        let Block::CodeBlock { content, .. } = std::mem::replace(block, Block::HorizontalRule)
        else {
            // Unreachable per the matcher above; a safe fallback rather than a panic.
            continue;
        };
        let source = DiagramSource::Mermaid(content);
        let url = crate::diagram::synthetic_url(&source);
        sources.insert(url.clone(), source);
        *block = Block::ImageBlock {
            alt: "mermaid diagram".to_string(),
            url,
        };
    }
    sources
}

/// `Some((alt, url))` iff `inlines` is exactly one `Inline::Image` plus optional
/// whitespace-only text and breaks.  Mixed content keeps its placeholder treatment.
fn extract_lone_image(inlines: &[Inline]) -> Option<(String, String)> {
    let mut image: Option<(String, String)> = None;
    for inline in inlines {
        match inline {
            Inline::Image { alt, url } => {
                if image.is_some() {
                    return None;
                }
                image = Some((alt.clone(), url.clone()));
            }
            Inline::Text(t) if t.trim().is_empty() => {}
            Inline::SoftBreak | Inline::HardBreak => {}
            _ => return None,
        }
    }
    image
}

/// Whether `body` is entirely well-formed `<!-- ... -->` comments plus whitespace.
/// Anything else is `false`, so the renderer still shows the raw source for it.
pub(super) fn is_html_comment_only(body: &str) -> bool {
    let mut rest = body.trim();
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        if !rest.starts_with("<!--") {
            return false;
        }
        // The closing `-->` must start at index 4 or later, so the delimiters can't
        // overlap on strings like `<!-->`.
        let Some(close) = rest[4..].find("-->") else {
            return false;
        };
        rest = rest[4 + close + 3..].trim_start();
    }
    true
}

/// Promote comment-only `Block::Html` into `Block::HtmlComment`.  The stored string keeps
/// its delimiters so downstream helpers need no variant-specific path.
pub fn promote_html_comments(blocks: &mut [Block]) {
    for block in blocks.iter_mut() {
        if let Block::Html(body) = block {
            if is_html_comment_only(body) {
                let body = std::mem::take(body);
                *block = Block::HtmlComment(body);
            }
        }
    }
}

/// Fold a `<!-- tui-columns: [..] -->` comment that directly follows a `Block::Table` into
/// that table's `user_widths`, removing the comment block.  Non-adjacent ones are left
/// intact and render as zero lines.
///
/// Must run AFTER [`promote_html_comments`], which is what creates the `HtmlComment`
/// blocks this consumes.
pub fn attach_trailing_tui_columns_comments(blocks: &mut Vec<Block>) {
    let mut i = 0;
    while i + 1 < blocks.len() {
        let is_pair = matches!(
            (&blocks[i], &blocks[i + 1]),
            (Block::Table { user_widths: None, .. }, Block::HtmlComment(body))
                if crate::markdown::table_layout::parse_column_widths_comment(body).is_some()
        );
        if is_pair {
            let body = match &blocks[i + 1] {
                Block::HtmlComment(s) => s.clone(),
                _ => unreachable!(),
            };
            let widths = crate::markdown::table_layout::parse_column_widths_comment(&body).unwrap();
            if let Block::Table { user_widths, .. } = &mut blocks[i] {
                *user_widths = Some(widths);
            }
            blocks.remove(i + 1);
            continue;
        }
        i += 1;
    }
}

/// Annotate each `ListItem` with the blank source lines directly preceding its marker
/// (`ListItem::blank_lines_before`).
///
/// CommonMark merges blank-separated items into one "loose" list; edamame wants those
/// blanks rendered but *without* fragmenting the block, so the list stays one
/// `Block::List` and the renderer emits the recorded blanks.  Keeping it whole is what
/// lets ordered numbering come straight from pulldown-cmark and keeps the block↔range
/// vectors 1:1.
///
/// `RenderedView`'s reveal maps rendered to source lines by splitting the block's raw text
/// on `\n`, so the recorded count must equal the blanks actually present: only a
/// contiguous run *directly* above item k counts, and blanks inside an embedded
/// `` ``` ``/`~~~` fence are skipped entirely.  `ranges` is read-only and stays 1:1.
///
/// Top-level lists only: a loose *nested* list renders tight.  Deliberate — nested support
/// would need nested range derivation plus multi-level indent tracking in the reveal
/// mapping, for a rare case.
pub fn annotate_list_blanks(blocks: &mut [Block], ranges: &[Range<usize>], source: &str) {
    for (block, range) in blocks.iter_mut().zip(ranges.iter()) {
        let Block::List { items, .. } = block else {
            continue;
        };
        let list_src = &source[range.clone()];
        let item_offsets = top_level_item_offsets(list_src);
        // If the source scan disagrees with the AST item count, leave the list untouched.
        if item_offsets.len() != items.len() {
            continue;
        }
        for k in 1..item_offsets.len() {
            let prev_line_end = line_end_in_str(list_src, item_offsets[k - 1]);
            let between_start = (prev_line_end + 1).min(item_offsets[k]);
            if let Some(gap_start) =
                separator_blank_run_start(list_src, between_start, item_offsets[k])
            {
                // The run is all blank lines by construction; each contributes one `\n`.
                items[k].blank_lines_before = list_src.as_bytes()[gap_start..item_offsets[k]]
                    .iter()
                    .filter(|&&b| b == b'\n')
                    .count();
            }
        }
    }
}

/// Byte offsets of every top-level item-start line in `list_src` — "top-level" meaning the
/// indent matches the first item's, so nested content is ignored.
fn top_level_item_offsets(list_src: &str) -> Vec<usize> {
    let bytes = list_src.as_bytes();
    let mut offsets = Vec::new();
    let mut first_indent: Option<String> = None;
    let mut pos = 0;
    while pos < bytes.len() {
        let line_end = line_end_in_str(list_src, pos);
        let line = &list_src[pos..line_end];
        if let Some((indent, _, _)) = parse_marker_line(line) {
            if first_indent.is_none() {
                first_indent = Some(indent.clone());
            }
            if first_indent.as_deref() == Some(indent.as_str()) {
                offsets.push(pos);
            }
        }
        pos = if line_end < bytes.len() {
            line_end + 1
        } else {
            line_end
        };
    }
    offsets
}

fn line_end_in_str(s: &str, start: usize) -> usize {
    let bytes = s.as_bytes();
    let mut p = start;
    while p < bytes.len() && bytes[p] != b'\n' {
        p += 1;
    }
    p
}

/// Byte offset where the run of blank lines *directly preceding* `end` starts, scanning
/// `[start, end)`.  `None` when the line above `end` isn't blank — blanks interior to the
/// previous item's content are not separators.  Blanks inside a `` ``` ``/`~~~` fence are
/// ignored, so an embedded code block never fragments its list.
fn separator_blank_run_start(s: &str, start: usize, end: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut pos = start;
    let mut fence: Option<(char, usize)> = None;
    let mut run_start: Option<usize> = None;
    while pos < end {
        let mut le = pos;
        while le < end && bytes[le] != b'\n' {
            le += 1;
        }
        let line = &s[pos..le];
        if let Some((fence_char, min_count)) = fence {
            if is_closing_fence(line, fence_char, min_count) {
                fence = None;
            }
            run_start = None;
        } else if let Some((c, count)) = parse_opening_fence(line) {
            fence = Some((c, count));
            run_start = None;
        } else if line.chars().all(char::is_whitespace) {
            run_start.get_or_insert(pos);
        } else {
            run_start = None;
        }
        pos = if le < end { le + 1 } else { le };
    }
    run_start
}

/// An opening fence marker: its character (`` ` `` or `~`) and run length.  Indentation of
/// any depth is permitted — inside a list item the fence sits at the content column, and
/// only the fence/no-fence state matters here.
pub fn parse_opening_fence(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let count = trimmed.chars().take_while(|&c| c == first).count();
    if count < 3 {
        return None;
    }
    // Backtick fences disallow backticks anywhere in the info string.
    if first == '`' && trimmed[count..].contains('`') {
        return None;
    }
    Some((first, count))
}

/// A closing fence for an open `fence_char` × `min_count`: same character, at least as
/// long, whitespace-only after it (CommonMark).
pub fn is_closing_fence(line: &str, fence_char: char, min_count: usize) -> bool {
    let trimmed = line.trim_start();
    let count = trimmed.chars().take_while(|&c| c == fence_char).count();
    if count < min_count {
        return false;
    }
    trimmed[count..].chars().all(char::is_whitespace)
}

/// The marker prefix of `line`: `(indent, marker_or_delim, number)`, the number being
/// `None` for bullets.  `None` when the line starts with no recognized marker.
fn parse_marker_line(line: &str) -> Option<(String, char, Option<u64>)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let indent = line[..i].to_owned();
    let rest = &line[i..];
    let rb = rest.as_bytes();
    if let Some(&c) = rb.first() {
        if matches!(c, b'-' | b'*' | b'+') && rb.get(1) == Some(&b' ') {
            return Some((indent, c as char, None));
        }
    }
    let digits_len = rb.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits_len > 0 {
        let num: u64 = rest[..digits_len].parse().ok()?;
        let delim = *rb.get(digits_len)?;
        if matches!(delim, b'.' | b')') && rb.get(digits_len + 1) == Some(&b' ') {
            return Some((indent, delim as char, Some(num)));
        }
    }
    None
}
