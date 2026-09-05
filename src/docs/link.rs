//! Resolve a relative link written inside an embedded documentation page. Pure and
//! I/O-free; the `#fragment` was already split off by `LinkTarget::parse`.
//!
//! Needed because a doc page is pathless: the ordinary local-file path would resolve
//! `security.md` against the process cwd. Interception lives in `App::follow_link`, gated
//! on a doc page being open, so `LinkTarget::parse` keeps answering the same way for a user
//! document that links to its own `security.md`.

use std::path::Path;

use super::registry::DocId;

const REPO_BLOB_BASE: &str = "https://github.com/mijowi/edamame/blob/main";

/// What a relative link inside a doc page turns out to name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocLinkResolution {
    /// Another embedded page, with any fragment carried through.
    Doc(DocId, Option<String>),
    /// A repository file not in the binary (`docs/dev/`, root `SECURITY.md`), as a GitHub URL.
    External(String),
}

/// An exact file-name match is another embedded page; anything else maps onto its GitHub
/// URL so the link still goes somewhere truthful.
pub fn resolve_doc_reference(path: &Path, fragment: Option<String>) -> DocLinkResolution {
    if let Some(id) = path.to_str().and_then(DocId::from_slug) {
        return DocLinkResolution::Doc(id, fragment);
    }
    let mut url = format!("{REPO_BLOB_BASE}/{}", repo_relative_path(path));
    if let Some(f) = fragment {
        url.push('#');
        url.push_str(&f);
    }
    DocLinkResolution::External(url)
}

/// Re-root a link relative to `docs/` onto a repository path (`../SECURITY.md` →
/// `SECURITY.md`, `dev/theming.md` → `docs/dev/theming.md`), textually — `canonicalize`
/// would consult an unrelated filesystem.
fn repo_relative_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    parts.push("docs");
    for segment in raw.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn resolve(p: &str, frag: Option<&str>) -> DocLinkResolution {
        resolve_doc_reference(&PathBuf::from(p), frag.map(str::to_owned))
    }

    #[test]
    fn a_sibling_page_resolves_to_that_page() {
        assert_eq!(
            resolve("security.md", None),
            DocLinkResolution::Doc(DocId::Security, None)
        );
    }

    #[test]
    fn a_fragment_is_carried_through_to_the_page() {
        assert_eq!(
            resolve("keybindings.md", Some("terminal-compatibility")),
            DocLinkResolution::Doc(
                DocId::Keybindings,
                Some("terminal-compatibility".to_owned())
            )
        );
    }

    #[test]
    fn a_contributor_page_becomes_a_github_url_under_docs() {
        assert_eq!(
            resolve("dev/theming.md", None),
            DocLinkResolution::External(format!("{REPO_BLOB_BASE}/docs/dev/theming.md"))
        );
    }

    #[test]
    fn a_parent_reference_climbs_out_of_the_docs_directory() {
        assert_eq!(
            resolve("../SECURITY.md", None),
            DocLinkResolution::External(format!("{REPO_BLOB_BASE}/SECURITY.md"))
        );
    }

    #[test]
    fn an_external_link_keeps_its_fragment() {
        assert_eq!(
            resolve("dev/security-invariants.md", Some("checklist")),
            DocLinkResolution::External(format!(
                "{REPO_BLOB_BASE}/docs/dev/security-invariants.md#checklist"
            ))
        );
    }

    #[test]
    fn an_unknown_file_name_does_not_masquerade_as_a_page() {
        assert_eq!(
            resolve("nonexistent.md", None),
            DocLinkResolution::External(format!("{REPO_BLOB_BASE}/docs/nonexistent.md"))
        );
    }

    #[test]
    fn every_cross_link_in_the_shipped_docs_resolves_somewhere_sane() {
        // Guards against a doc being renamed out from under a sibling's link.
        for page in super::super::registry::ALL_DOCS {
            for target in md_link_targets(page.source) {
                let (path, _) = match target.split_once('#') {
                    Some((p, f)) => (p, Some(f)),
                    None => (target.as_str(), None),
                };
                if path.is_empty() {
                    continue; // a same-page `#anchor`
                }
                let resolved = resolve(path, None);
                if let DocLinkResolution::External(url) = &resolved {
                    assert!(
                        url.contains("/docs/dev/") || url.ends_with("/SECURITY.md"),
                        "{} links to {path}, which resolves to an unexpected {url}",
                        page.slug
                    );
                }
            }
        }
    }

    /// Every `](target)` that looks like a local Markdown path; deliberately not a parser.

    fn md_link_targets(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        let bytes: Vec<char> = src.chars().collect();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == ']' && bytes[i + 1] == '(' {
                let start = i + 2;
                let mut j = start;
                while j < bytes.len() && bytes[j] != ')' {
                    j += 1;
                }
                if j < bytes.len() {
                    let target: String = bytes[start..j].iter().collect();
                    if target.contains(".md") && !target.starts_with("http") {
                        out.push(target);
                    }
                }
                i = j;
            }
            i += 1;
        }
        out
    }
}
