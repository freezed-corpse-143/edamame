use pulldown_cmark::HeadingLevel;

// ─── Block-level nodes ────────────────────────────────────────────────────────

// `CodeBlock` / `BlockQuote` are Markdown terminology, not stuttering.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Block {
    Heading {
        level: HeadingLevel,
        inlines: Vec<Inline>,
    },
    Paragraph {
        inlines: Vec<Inline>,
    },
    CodeBlock {
        language: Option<String>,
        content: String,
        fenced: bool,
    },
    BlockQuote {
        blocks: Vec<Block>,
    },
    List {
        ordered: bool,
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    HorizontalRule,
    Table {
        /// Column count (from the GFM table alignment row).
        col_count: usize,
        headers: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
        /// Column widths from a trailing `<!-- tui-columns: [..] -->` comment (stripped from the
        /// AST by the parser). Outer `None` = no comment; inner `None` (`_` in the comment) =
        /// auto-size that column.
        user_widths: Option<Vec<Option<usize>>>,
    },
    /// Raw HTML, rendered as a plain fenced block.
    Html(String),
    /// An HTML comment promoted out of `Block::Html` by the parser post-pass. Stores the full
    /// source including delimiters (same convention as `Html`, so comment helpers accept either).
    /// Renders zero lines; its bytes are still covered by the `SourceMap`.
    HtmlComment(String),
    /// A paragraph whose sole content is an image, promoted so the renderer can reserve a
    /// multi-row region for the graphics overlay. Mixed paragraphs keep `Inline::Image`
    /// placeholders since graphics can't sit mid-wrap.
    ImageBlock {
        alt: String,
        url: String,
    },
    /// YAML (`---`) or TOML (`+++`) frontmatter, recognized only where CommonMark's metadata
    /// extension accepts one. `content` is the raw text between the delimiter lines, never
    /// re-flowed; the delimiters are reproduced from `kind` (see `docs/dev/frontmatter.md`).
    MetadataBlock {
        kind: MetadataKind,
        content: String,
    },
    /// A footnote definition, rendered in place at its source position with the raw `label` as
    /// marker so the rendered number never diverges from the source. Renumbering is the
    /// `RenumberFootnotes` action's job, not the renderer's.
    FootnoteDefinition {
        label: String,
        blocks: Vec<Block>,
    },
}

/// Which delimiter style opened a [`Block::MetadataBlock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetadataKind {
    Yaml,
    Toml,
}

impl MetadataKind {
    pub fn delimiter(self) -> &'static str {
        match self {
            MetadataKind::Yaml => "---",
            MetadataKind::Toml => "+++",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ListItem {
    pub blocks: Vec<Block>,
    /// `Some(true)` = checked, `Some(false)` = unchecked, `None` = not a task item.
    pub task: Option<bool>,
    /// Blank source lines directly before this item's marker (outside fences); `0` for the first
    /// item. Set by [`crate::markdown::parser::post_pass::annotate_list_blanks`] so a loose list
    /// keeps its spacing while staying one `Block::List`.
    pub blank_lines_before: usize,
}

// ─── Inline nodes ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    Code(String),
    Link {
        text: Vec<Inline>,
        url: String,
        title: Option<String>,
    },
    Image {
        alt: String,
        url: String,
    },
    Highlight(Vec<Inline>),
    /// Mid-paragraph HTML comment, stored with delimiters; renders as zero spans.
    HtmlComment(String),
    /// A footnote reference (`[^label]`). Always has a definition: pulldown-cmark leaves an
    /// undefined `[^x]` as literal text. See `docs/dev/footnotes.md` for marker rendering.
    FootnoteReference {
        label: String,
    },
    SoftBreak,
    HardBreak,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// `inlines_to_plain` with hard breaks collapsed to spaces, for single-row uses.
pub fn heading_plain_text(inlines: &[Inline]) -> String {
    inlines_to_plain(inlines).replace('\n', " ")
}

/// Flatten inlines to a plain text string (no styling).
pub fn inlines_to_plain(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(t) => out.push_str(t),
            Inline::Bold(inner)
            | Inline::Italic(inner)
            | Inline::Strikethrough(inner)
            | Inline::Highlight(inner) => {
                out.push_str(&inlines_to_plain(inner));
            }
            Inline::Code(c) => out.push_str(c),
            Inline::Link { text, .. } => out.push_str(&inlines_to_plain(text)),
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::HtmlComment(_) => {}
            // Footnote markers are chrome, not prose: keep them out of heading slugs.
            Inline::FootnoteReference { .. } => {}
            Inline::SoftBreak => out.push(' '),
            Inline::HardBreak => out.push('\n'),
        }
    }
    out
}
