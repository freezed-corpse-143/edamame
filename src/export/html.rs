use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use pulldown_cmark::{
    html as cmark_html, CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd,
};

use super::runner::{write_atomically, ExportOutcome};
use crate::diagram;
use crate::image::{rasterize_svg, SvgScaleMode, SvgSizing};

/// The compiled-in stylesheet, used for [`Stylesheet::Builtin`].
pub const BUILTIN_STYLESHEET: &str = include_str!("../../config/export/default.css");

/// Source of the CSS embedded in the generated HTML document.
#[derive(Debug, Clone)]
pub enum Stylesheet {
    /// The bundled `config/export/default.css`.
    Builtin,
    /// Read a user CSS file at export time.
    Path(PathBuf),
    /// CSS verbatim.  Tests and embeddings only — the binary builds `Builtin` / `Path`.
    #[allow(dead_code)]
    Inline(String),
}

impl Stylesheet {
    /// Parse `[export.html].stylesheet`: the sentinel `"builtin"`, or a filesystem path.
    pub fn from_config_value(value: &str) -> Self {
        if value.eq_ignore_ascii_case("builtin") {
            Self::Builtin
        } else {
            Self::Path(PathBuf::from(value))
        }
    }

    fn load(&self) -> Result<String> {
        match self {
            Self::Builtin => Ok(BUILTIN_STYLESHEET.to_owned()),
            Self::Path(p) => std::fs::read_to_string(p)
                .with_context(|| format!("Failed to read stylesheet: {}", p.display())),
            Self::Inline(s) => Ok(s.clone()),
        }
    }
}

/// Options passed to [`render_html`] / [`spawn_html_export`].
#[derive(Debug, Clone)]
pub struct HtmlExportOptions {
    /// Source of the embedded CSS.
    pub stylesheet: Stylesheet,
    /// Embed relative image references as `data:` URIs so the HTML is self-contained.  Requires
    /// `source_dir`; remote and already-`data:` URLs are untouched either way.
    pub inline_images: bool,
    /// Resolves relative image paths, and bounds them: see [`resolve_relative`].  `None`
    /// disables the rewrite even when `inline_images` is true.
    pub source_dir: Option<PathBuf>,
    /// `<title>` text; `None` falls back to `"Document"`.
    pub title: Option<String>,
    /// Render mermaid fences to a `<figure class="mermaid-diagram">`, falling back to the usual
    /// code block on failure so the source is never lost.
    pub render_diagrams: bool,
}

impl Default for HtmlExportOptions {
    fn default() -> Self {
        Self {
            stylesheet: Stylesheet::Builtin,
            inline_images: false,
            source_dir: None,
            title: None,
            render_diagrams: true,
        }
    }
}

/// Render `markdown` to a standalone HTML document, mirroring the in-app renderer's parser
/// options so an export looks like the terminal preview.
///
/// **Raw HTML events are filtered out before serialization** — block *and* inline — so
/// attacker-controlled Markdown cannot inject `<script>` or other executable content.
pub fn render_html(markdown: &str, opts: &HtmlExportOptions) -> Result<String> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);
    // Without the frontmatter extension a `---` block parses as a thematic break plus a setext
    // H2, and the export opens with the YAML keys as its loudest heading.  It is gated on *this*
    // document's opening delimiter, through the shared `metadata_options_for`: the extensions are
    // not anchored to the document start on their own, so leaving them on unconditionally would
    // let a mid-document `---` claim the section under it — and the writer emits nothing for a
    // metadata block, so that section would vanish from the export silently.
    options |= crate::markdown::parse_offsets::metadata_options_for(markdown);

    let parser = Parser::new_ext(markdown, options);

    // Collected so the image-rewrite pass can mutate events in place.
    let mut events: Vec<Event> = parser
        .filter(|e| !matches!(e, Event::Html(_) | Event::InlineHtml(_)))
        .collect();

    if opts.inline_images {
        if let Some(dir) = opts.source_dir.as_deref() {
            rewrite_images_to_data_uris(&mut events, dir);
        }
    }

    if opts.render_diagrams {
        events = replace_mermaid_with_image(events);
    }

    // pulldown-cmark's HTML writer performs no URL sanitization, so without this a
    // `[x](javascript:…)` link survives into the exported `<a href>` and runs on click.
    sanitize_link_urls(&mut events);

    let mut body = String::new();
    cmark_html::push_html(&mut body, events.into_iter());

    let css = opts.stylesheet.load()?;
    let title = opts.title.as_deref().unwrap_or("Document");

    Ok(format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n\
         <style>\n{css}\n</style>\n\
         </head>\n\
         <body>\n\
         <main class=\"markdown-body\">\n\
         {body}\n\
         </main>\n\
         </body>\n\
         </html>\n",
        title = html_escape(title),
    ))
}

/// Render `markdown` to `target` on a worker thread, invoking the closure there with the outcome.
///
/// **The caller must run [`crate::export::preflight`] first** — this clobbers an existing
/// `target`.
pub fn spawn_html_export(
    markdown: String,
    target: PathBuf,
    opts: HtmlExportOptions,
    on_done: impl FnOnce(ExportOutcome) + Send + 'static,
) {
    std::thread::spawn(move || {
        let result = render_and_write(&markdown, &target, &opts).map(|()| target.clone());
        on_done(result.map_err(|e| format!("{e:#}")));
    });
}

fn render_and_write(markdown: &str, target: &Path, opts: &HtmlExportOptions) -> Result<()> {
    let html = render_html(markdown, opts)?;
    write_atomically(target, html.as_bytes())
        .with_context(|| format!("Failed to write export: {}", target.display()))?;
    Ok(())
}

// ── Link URL sanitization ─────────────────────────────────────────────────

/// Schemes permitted on an exported link destination; everything else is neutralized.
const SAFE_LINK_SCHEMES: &[&str] = &["http", "https", "mailto", "tel"];

/// Rewrite every link destination outside [`SAFE_LINK_SCHEMES`] to a harmless `#`.  Relative
/// paths and anchors carry no scheme and are untouched.
fn sanitize_link_urls(events: &mut [Event<'_>]) {
    for event in events.iter_mut() {
        if let Event::Start(Tag::Link { dest_url, .. }) = event {
            if !is_safe_link_url(dest_url.as_ref()) {
                *dest_url = CowStr::Borrowed("#");
            }
        }
    }
}

/// True when `url` has no scheme at all or an allowlisted one.  A "scheme" is an RFC-3986 token
/// terminated by `:` *before* any `/`, `?`, or `#`; a later colon is part of the path
/// (`foo/bar:baz`) and makes no scheme.
fn is_safe_link_url(url: &str) -> bool {
    let url = url.trim();
    let Some(idx) = url.find([':', '/', '?', '#']) else {
        return true; // no delimiter at all → relative
    };
    if url.as_bytes()[idx] != b':' {
        return true; // a path/query/fragment delimiter came first → relative
    }
    let scheme = &url[..idx];
    let scheme_shaped = scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !scheme_shaped {
        // Not a real scheme (a port-looking path segment) → relative.
        return true;
    }
    SAFE_LINK_SCHEMES
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s))
}

// ── Mermaid diagrams ──────────────────────────────────────────────────────

/// Replace each mermaid fence with a single `Event::Html` figure, preserving the original events
/// on render failure so the diagram source is never lost.
///
/// **The diagram is rasterized to a PNG `data:` image, never inlined as `<svg>`.**  Inline SVG can
/// carry `<script>`, `foreignObject`, and `on*=` handlers that execute when the export is opened
/// in a browser; flattening to pixels means no executable markup from the document-controlled,
/// third-party-rendered SVG can survive.
///
/// Language matching is case-insensitive, like the in-app `promote_diagram_code_blocks`.
fn replace_mermaid_with_image(events: Vec<Event<'_>>) -> Vec<Event<'_>> {
    let mut out: Vec<Event<'_>> = Vec::with_capacity(events.len());
    let mut iter = events.into_iter();
    while let Some(event) = iter.next() {
        let lang = match &event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang)))
                if lang.as_ref().eq_ignore_ascii_case("mermaid") =>
            {
                Some(lang.clone())
            }
            _ => None,
        };
        if lang.is_none() {
            out.push(event);
            continue;
        }
        // Collect Text events to the matching end, then either emit one `Event::Html` or replay
        // the originals for the default serializer's fallback.
        let mut buffered: Vec<Event<'_>> = vec![event];
        let mut source = String::new();
        for inner in iter.by_ref() {
            match inner {
                Event::End(TagEnd::CodeBlock) => {
                    buffered.push(Event::End(TagEnd::CodeBlock));
                    break;
                }
                Event::Text(ref t) => {
                    source.push_str(t);
                    buffered.push(inner);
                }
                other => {
                    // Shouldn't occur inside a fenced code block; treat as text and preserve.
                    buffered.push(other);
                }
            }
        }
        match render_mermaid_png_data_uri(&source) {
            Some(data_uri) => {
                let html = format!(
                    "<figure class=\"mermaid-diagram\">\
                     <img alt=\"mermaid diagram\" src=\"{data_uri}\">\
                     </figure>"
                );
                out.push(Event::Html(CowStr::Boxed(html.into_boxed_str())));
            }
            None => {
                // Fall back to the code block; a per-diagram failure is not fatal to the export.
                out.extend(buffered);
            }
        }
    }
    out
}

/// Render mermaid `source` to a PNG `data:` URI, or `None` on any failure.  The intermediate SVG
/// never reaches the HTML — rasterizing strips any script / `foreignObject` / event-handler
/// payload a hostile node label smuggled through the renderer's escaping.
fn render_mermaid_png_data_uri(source: &str) -> Option<String> {
    let svg = diagram::render_mermaid_svg(source).ok()?;
    let image = rasterize_svg(
        &svg,
        SvgSizing {
            envelope: None,
            font_size: None,
            mode: SvgScaleMode::Natural,
        },
        Some([255, 255, 255, 255]),
    )
    .ok()?;
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        BASE64.encode(png.into_inner())
    ))
}

// ── Image inlining ────────────────────────────────────────────────────────

fn rewrite_images_to_data_uris(events: &mut [Event<'_>], source_dir: &Path) {
    for event in events.iter_mut() {
        if let Event::Start(Tag::Image { dest_url, .. }) = event {
            if let Some(new_url) = inline_image_data_uri(dest_url.as_ref(), source_dir) {
                *dest_url = CowStr::Boxed(new_url.into_boxed_str());
            }
        }
    }
}

/// A `data:` URI for `url` if it resolves to a readable local image.  `None` means "leave as-is":
/// remote URLs, existing `data:` URIs, and anything unreadable or unclassifiable.
fn inline_image_data_uri(url: &str, source_dir: &Path) -> Option<String> {
    if is_remote_url(url) {
        return None;
    }
    let path = resolve_relative(url, source_dir)?;
    let bytes = std::fs::read(&path).ok()?;
    let mime = mime_from_extension(&path)?;
    let mut encoded = String::from("data:");
    encoded.push_str(mime);
    encoded.push_str(";base64,");
    encoded.push_str(&BASE64.encode(&bytes));
    Some(encoded)
}

fn is_remote_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("data:")
        || lower.starts_with("file://")
}

/// Resolve a relative image `url` against `source_dir`, **only if it stays within it**.  A
/// self-contained export is an artifact the victim shares, so an out-of-tree path would let a
/// hostile document exfiltrate arbitrary files base64-encoded into that output.  Absolute paths
/// and `..` components are rejected up front; the post-`canonicalize` containment check defeats
/// symlink escapes.
///
/// `None` leaves the original reference in place — the export doesn't embed it rather than leaking
/// it.
fn resolve_relative(url: &str, source_dir: &Path) -> Option<PathBuf> {
    let p = Path::new(url);
    if p.is_absolute() {
        return None;
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    let joined = source_dir.join(p);
    let canon_dir = source_dir.canonicalize().ok()?;
    let canon = joined.canonicalize().ok()?;
    canon.starts_with(&canon_dir).then_some(canon)
}

fn mime_from_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

// ── HTML escaping ─────────────────────────────────────────────────────────

/// Escape the five XML metacharacters.  Only for `<title>`; the body is escaped by
/// `pulldown_cmark::html`.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn opts_inline_css() -> HtmlExportOptions {
        HtmlExportOptions {
            stylesheet: Stylesheet::Inline("body { color: red; }".into()),
            ..HtmlExportOptions::default()
        }
    }

    #[test]
    fn renders_basic_markdown() {
        let html = render_html("# Hello\n\nWorld", &opts_inline_css()).unwrap();
        assert!(html.contains("<h1>Hello</h1>"));
        assert!(html.contains("<p>World</p>"));
    }

    /// Without the extension the export reproduces the rule-plus-setext-H2 misparse.
    #[test]
    fn frontmatter_is_omitted_from_the_export() {
        let md = "---\ntitle: Foo\ndate: 2026-01-01\n---\n\n# Heading\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("<h1>Heading</h1>"));
        assert!(!html.contains("title: Foo"), "got: {html}");
        assert!(!html.contains("<h2>"), "got: {html}");
    }

    /// The writer emits *nothing* for a metadata block, so an unanchored extension would drop a
    /// section a mid-document `---` pair brackets — silently, and only in the export.
    #[test]
    fn a_mid_document_rule_pair_is_not_dropped_from_the_export() {
        let md = "Intro.\n\n---\n## Section 2\n\nText.\n\n---\n## Section 3\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("Section 2"), "got: {html}");
        assert!(html.contains("Text."), "got: {html}");
        assert!(html.contains("Section 3"), "got: {html}");
    }

    /// The export's gate must be the renderer's, or the two disagree about what frontmatter is.
    #[test]
    fn a_toml_opening_file_does_not_drop_a_later_dash_pair() {
        let md = "+++\na = 1\n+++\n\n---\nSection\n---\n\nEnd.\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(!html.contains("a = 1"), "got: {html}");
        assert!(html.contains("Section"), "got: {html}");
    }

    #[test]
    fn renders_gfm_table() {
        let md = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("<table>"));
        assert!(html.contains("<th>a</th>"));
        assert!(html.contains("<td>1</td>"));
    }

    #[test]
    fn renders_task_list_and_strikethrough() {
        let md = "- [x] done\n- [ ] todo\n\n~~gone~~";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("type=\"checkbox\""));
        assert!(html.contains("<del>gone</del>"));
    }

    #[test]
    fn strips_raw_html_block() {
        let md = "text\n\n<script>alert('x')</script>\n\nmore";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(
            !html.contains("<script>"),
            "raw <script> must be stripped — got:\n{html}"
        );
    }

    #[test]
    fn strips_raw_html_inline() {
        let md = "a <b onclick=\"x\">inline</b> c";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(
            !html.contains("onclick"),
            "inline HTML event handlers must be stripped — got:\n{html}"
        );
    }

    #[test]
    fn escapes_title() {
        let mut opts = opts_inline_css();
        opts.title = Some("A <script>x</script> & B".into());
        let html = render_html("", &opts).unwrap();
        assert!(html.contains("<title>A &lt;script&gt;x&lt;/script&gt; &amp; B</title>"));
        assert!(!html.contains("<title>A <script>"));
    }

    #[test]
    fn embeds_builtin_stylesheet() {
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Builtin,
            ..HtmlExportOptions::default()
        };
        let html = render_html("hi", &opts).unwrap();
        assert!(html.contains("markdown-body"));
        assert!(html.contains("<style>"));
    }

    #[test]
    fn footnotes_render_with_bracket_convention() {
        // The bundled CSS adds the `[ ]` brackets by targeting this exact markup.
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Builtin,
            ..HtmlExportOptions::default()
        };
        let html = render_html("Claim.[^1]\n\n[^1]: Source.\n", &opts).unwrap();
        assert!(
            html.contains("<sup class=\"footnote-reference\"><a href=\"#1\">1</a></sup>"),
            "expected footnote-reference markup, got:\n{html}"
        );
        assert!(
            html.contains("sup.footnote-reference a::before { content: \"[\"; }"),
            "builtin CSS must add the opening bracket"
        );
        assert!(
            html.contains("sup.footnote-reference a::after { content: \"]\"; }"),
            "builtin CSS must add the closing bracket"
        );
    }

    #[test]
    fn stylesheet_from_config_value_parses() {
        assert!(matches!(
            Stylesheet::from_config_value("builtin"),
            Stylesheet::Builtin
        ));
        assert!(matches!(
            Stylesheet::from_config_value("BUILTIN"),
            Stylesheet::Builtin
        ));
        match Stylesheet::from_config_value("/etc/custom.css") {
            Stylesheet::Path(p) => assert_eq!(p, PathBuf::from("/etc/custom.css")),
            other => panic!("expected Path, got {other:?}"),
        }
    }

    #[test]
    fn stylesheet_path_read_failure_surfaces_error() {
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Path(PathBuf::from("/this/does/not/exist.css")),
            ..HtmlExportOptions::default()
        };
        let err = render_html("hi", &opts).unwrap_err();
        assert!(format!("{err:#}").contains("stylesheet"));
    }

    #[test]
    fn inline_images_embeds_local_png() {
        // 1x1 transparent PNG
        const ONE_PX_PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("pixel.png");
        let mut f = std::fs::File::create(&img_path).unwrap();
        f.write_all(ONE_PX_PNG).unwrap();
        drop(f);

        let md = "![pixel](pixel.png)";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            inline_images: true,
            source_dir: Some(dir.path().to_path_buf()),
            title: None,
            render_diagrams: false,
        };
        let html = render_html(md, &opts).unwrap();
        assert!(
            html.contains("src=\"data:image/png;base64,"),
            "expected base64 data URI, got:\n{html}"
        );
        assert!(!html.contains("src=\"pixel.png\""));
    }

    #[test]
    fn inline_images_leaves_remote_urls_untouched() {
        let md = "![cat](https://example.com/cat.png)";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            inline_images: true,
            source_dir: Some(PathBuf::from("/tmp")),
            title: None,
            render_diagrams: false,
        };
        let html = render_html(md, &opts).unwrap();
        assert!(html.contains("src=\"https://example.com/cat.png\""));
    }

    #[test]
    fn inline_images_disabled_by_default() {
        let md = "![x](local.png)";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("src=\"local.png\""));
    }

    // ── Vuln 2: link-scheme sanitization ──────────────────────────────

    #[test]
    fn neutralizes_javascript_link_scheme() {
        let md = "[click](javascript:alert(document.cookie))";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(
            !html.contains("javascript:"),
            "javascript: href must be neutralized — got:\n{html}"
        );
        assert!(html.contains("href=\"#\""));
    }

    #[test]
    fn neutralizes_data_html_link_scheme() {
        let md = "[x](data:text/html;base64,PHNjcmlwdD4=)";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(
            !html.contains("data:text/html"),
            "data: link must be neutralized:\n{html}"
        );
    }

    #[test]
    fn preserves_safe_link_schemes_and_relative_targets() {
        let md = "[a](https://example.com) [b](mailto:x@y.z) [c](./page.md) [d](#anchor) [e](foo/bar:baz)";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("href=\"https://example.com\""));
        assert!(html.contains("href=\"mailto:x@y.z\""));
        assert!(html.contains("href=\"./page.md\""));
        assert!(html.contains("href=\"#anchor\""));
        // A colon after a path segment is not a scheme.
        assert!(html.contains("href=\"foo/bar:baz\""));
    }

    #[test]
    fn is_safe_link_url_classifies_schemes() {
        assert!(is_safe_link_url("https://example.com"));
        assert!(is_safe_link_url("HTTP://EXAMPLE.COM"));
        assert!(is_safe_link_url("mailto:a@b.c"));
        assert!(is_safe_link_url("/abs/path"));
        assert!(is_safe_link_url("./rel"));
        assert!(is_safe_link_url("#frag"));
        assert!(is_safe_link_url("?q=1"));
        assert!(is_safe_link_url("path/to:thing"));
        assert!(!is_safe_link_url("javascript:alert(1)"));
        assert!(!is_safe_link_url("  javascript:alert(1)"));
        assert!(!is_safe_link_url("vbscript:msgbox"));
        assert!(!is_safe_link_url("data:text/html,x"));
        assert!(!is_safe_link_url("file:///etc/passwd"));
    }

    // ── Vuln 3: mermaid export carries no raw SVG / script ─────────────

    #[test]
    fn mermaid_export_never_emits_raw_svg_or_script() {
        // Holds whether or not the live renderer is available: a success rasterizes to PNG, a
        // failure falls back to an escaped code block.
        let md = "```mermaid\nflowchart TD\n  A[\"<script>alert(1)</script>\"] --> B\n```";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            render_diagrams: true,
            ..HtmlExportOptions::default()
        };
        let html = render_html(md, &opts).unwrap();
        assert!(
            !html.contains("<svg"),
            "no raw SVG may reach the export:\n{html}"
        );
        assert!(!html.contains("foreignObject"));
        assert!(
            !html.contains("<script>"),
            "no executable <script> may reach the export:\n{html}"
        );
    }

    // ── Vuln 4: image inlining stays within the source tree ────────────

    fn write_one_px_png(path: &Path) {
        const ONE_PX_PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        std::fs::write(path, ONE_PX_PNG).unwrap();
    }

    #[test]
    fn inline_images_rejects_absolute_path() {
        let outside = tempdir().unwrap();
        let secret = outside.path().join("secret.png");
        write_one_px_png(&secret);
        let source = tempdir().unwrap();

        let md = format!("![x]({})", secret.display());
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            inline_images: true,
            source_dir: Some(source.path().to_path_buf()),
            render_diagrams: false,
            ..HtmlExportOptions::default()
        };
        let html = render_html(&md, &opts).unwrap();
        assert!(
            !html.contains("data:image/png"),
            "absolute out-of-tree path must not be inlined:\n{html}"
        );
    }

    #[test]
    fn inline_images_rejects_parent_traversal() {
        let root = tempdir().unwrap();
        let secret = root.path().join("secret.png");
        write_one_px_png(&secret);
        let source = root.path().join("docs");
        std::fs::create_dir(&source).unwrap();

        let md = "![x](../secret.png)";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            inline_images: true,
            source_dir: Some(source.clone()),
            render_diagrams: false,
            ..HtmlExportOptions::default()
        };
        let html = render_html(md, &opts).unwrap();
        assert!(
            !html.contains("data:image/png"),
            "../ traversal must not be inlined:\n{html}"
        );
    }

    #[test]
    fn spawn_html_export_writes_file_and_reports_success() {
        use std::sync::mpsc;
        let dir = tempdir().unwrap();
        let target = dir.path().join("out.html");
        let (tx, rx) = mpsc::channel();
        spawn_html_export(
            "# hi\n".into(),
            target.clone(),
            HtmlExportOptions {
                stylesheet: Stylesheet::Inline("body{}".into()),
                ..HtmlExportOptions::default()
            },
            move |outcome| {
                tx.send(outcome).unwrap();
            },
        );
        let outcome = rx.recv().unwrap();
        assert_eq!(outcome.unwrap(), target);
        let written = std::fs::read_to_string(&target).unwrap();
        assert!(written.contains("<h1>hi</h1>"));
    }
}
