//! Body content for the update-check modal.
//!
//! A pure content builder: the modal adapter owns the status and spinner timing and calls
//! [`body_lines`] each frame with plain values, so nothing here depends on the `app` layer.
//! [`UpdateReport`] is the `ui` layer's own vocabulary for that reason.
//!
//! Release notes arrive already truncated, control-stripped and capped by
//! `app::update_check::parse::sanitize_notes`, and get *structural* styling only — see
//! [`note_lines`].  They are **never** re-parsed as Markdown: this is network text, and a
//! parser would let a release body choose emphasis, links, images, and layout inside an app
//! modal.  `**bold**` and `[text](url)` stay literal, which is what the release page says.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::config::Theme;

/// Braille spinner frames, advanced by the modal while a check is in flight.
pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// What the modal is reporting right now.
pub enum UpdateReport<'a> {
    /// A check is in flight; nothing conclusive to show yet.
    Checking { spinner_frame: usize },
    /// The latest release is not newer than the installed build.
    UpToDate { tag: &'a str },
    /// A newer release exists, with whatever notes it carried.
    Available { tag: &'a str, notes: &'a [String] },
    /// A release was found but the versions could not be ordered (a pre-release suffix, or a
    /// tag not shaped like a version).  Both numbers are shown and the verdict says so, rather
    /// than claiming "up to date" over two rows that disagree.
    Inconclusive { tag: &'a str },
    /// The check could not be completed.
    Failed,
}

/// Left column of the two version rows: the longer label plus a separating space, so the
/// values line up.
const VERSION_LABEL_WIDTH: usize = "Installed version: ".len();

/// Label on the post-upgrade notice's version row.  Names the program rather than saying
/// "installed", since it renders only for [`PostUpgradeOccasion::OnDemand`].
const VERSION_LABEL: &str = "edamame version:";

/// Build the modal body.  `installed` is the bare Cargo version; the `v` prefix is added here
/// so it reads like a tag.
///
/// Both conclusive states share one shape — a verdict line, then the two version rows — so the
/// numbers are read off a table and moving between states doesn't rearrange the page.
pub fn body_lines(theme: &Theme, report: UpdateReport<'_>, installed: &str) -> Vec<Line<'static>> {
    match report {
        UpdateReport::Checking { spinner_frame } => {
            let frame = SPINNER_FRAMES[spinner_frame % SPINNER_FRAMES.len()];
            vec![Line::from(Span::styled(
                format!("Checking for updates… {frame}"),
                theme.modal_description,
            ))]
        }
        UpdateReport::UpToDate { tag } => {
            let mut out = vec![
                Line::from(Span::styled("edamame is up to date.".to_owned(), theme.h1)),
                Line::raw(""),
            ];
            out.extend(version_rows(theme, installed, tag));
            out
        }
        UpdateReport::Inconclusive { tag } => {
            let mut out = vec![
                Line::from(Span::styled(
                    "Couldn't compare versions.".to_owned(),
                    theme.h1,
                )),
                Line::raw(""),
            ];
            out.extend(version_rows(theme, installed, tag));
            out.push(Line::raw(""));
            out.push(Line::from(Span::styled(
                "The latest release isn't named like a plain version number, so \
                 edamame can't tell whether it is newer. Check the release page \
                 to be sure."
                    .to_owned(),
                theme.modal_description,
            )));
            out
        }
        UpdateReport::Failed => vec![
            Line::from(Span::styled(
                "Couldn't check for updates.".to_owned(),
                theme.h1,
            )),
            Line::raw(""),
            Line::from(Span::styled(
                "GitHub could not be reached. Your installed version is unchanged.".to_owned(),
                theme.modal_description,
            )),
        ],
        UpdateReport::Available { tag, notes } => {
            let mut out = vec![
                Line::from(Span::styled("Update available.".to_owned(), theme.h1)),
                Line::raw(""),
            ];
            out.extend(version_rows(theme, installed, tag));
            // No notes: emit nothing rather than a "What's new" heading over blank space.
            if !notes.is_empty() {
                out.push(Line::raw(""));
                out.extend(note_lines(notes));
            }
            out
        }
    }
}

/// What the post-upgrade notice reports — the one-time post-update modal and the About page's
/// `[ Release notes ]` button.
///
/// A second enum rather than a variant on [`UpdateReport`]: that one reports a *check* against
/// GitHub with two versions to compare, this one reports on the running build and has one.
/// They share this module so what they put on screen can't drift.
pub enum PostUpgradeReport<'a> {
    /// The changelog carried a section for this version; `notes` may still be empty.
    Found { notes: &'a [String] },
    /// No `## [<version>]` section for this build.  Only the on-demand path renders it; the
    /// startup notice stays silent, since the user already knows they upgraded.
    NotFound,
}

/// Which entry point is asking — the only difference between the two bodies.
///
/// One body serves both, but the opening line cannot: the startup notice fires *because* the
/// build changed and says so, while the About page's button asks about the build already
/// running, where "Updated to …" would announce an event that never happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostUpgradeOccasion {
    /// The one-time startup notice: this build is newer than the one that last ran.
    Upgrade,
    /// The About page's `[ Release notes ]` button.
    OnDemand,
}

/// Build the post-upgrade body.  The opening line is the [`PostUpgradeOccasion`]'s: a verdict
/// for the upgrade notice, a labeled version row for the on-demand opening.  Neither carries
/// the other's — the headline already names the number, and there is no occasion to name on
/// demand.  One row, not [`body_lines`]'s two, since there is nothing to compare against.
pub fn post_upgrade_body_lines(
    theme: &Theme,
    occasion: PostUpgradeOccasion,
    report: PostUpgradeReport<'_>,
    installed: &str,
) -> Vec<Line<'static>> {
    let mut out = vec![match occasion {
        PostUpgradeOccasion::Upgrade => {
            Line::from(Span::styled(format!("Updated to v{installed}."), theme.h1))
        }
        // One space, not the update check's shared column: this row stands alone.
        PostUpgradeOccasion::OnDemand => version_row(
            theme,
            VERSION_LABEL,
            format!("v{installed}"),
            VERSION_LABEL.len() + 1,
        ),
    }];
    match report {
        PostUpgradeReport::NotFound => {
            out.push(Line::raw(""));
            out.push(Line::from(Span::styled(
                "No release notes are bundled for this version.".to_owned(),
                theme.modal_description,
            )));
        }
        // As in the `Available` arm: a present-but-empty section gets no heading.
        PostUpgradeReport::Found { notes } if !notes.is_empty() => {
            out.push(Line::raw(""));
            out.extend(note_lines(notes));
        }
        PostUpgradeReport::Found { .. } => {}
    }
    out
}

/// Glyph substituted for a list marker (`-`, `*`, `+`).
const BULLET: &str = "•";

/// Render the release notes with *structural* styling only: an ATX heading loses its `#` run
/// and is bolded, a list marker becomes a [`BULLET`], and Keep a Changelog's blank line under
/// every heading is dropped.  Everything else is verbatim.
///
/// Deliberately not Markdown rendering, and that distinction is the whole safety argument: each
/// *line* is classified locally, over text `parse::sanitize_notes` has already bounded, into one
/// of three fixed local styles.  A release body can make a line a heading; it cannot make it
/// anything this function doesn't already know how to draw.  Don't grow this into an inline
/// parser — `**bold**` and `[text](url)` stay literal on purpose.
///
/// A wrapped line's continuation starts at column 0: `ModalView` owns the wrapping, so a
/// hanging indent would require pre-wrapping the body, which double-wraps at narrow widths.
fn note_lines(notes: &[String]) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(notes.len());
    let mut after_heading = false;
    for raw in notes {
        if let Some(text) = heading_text(raw) {
            // The blank *before* a heading separates sections and is kept; the one after it is
            // Keep a Changelog's shape, and rows are scarce in a modal.
            out.push(Line::from(Span::styled(
                text.to_owned(),
                Style::new().add_modifier(Modifier::BOLD),
            )));
            after_heading = true;
            continue;
        }
        if raw.trim().is_empty() {
            let previous_was_blank = out.last().is_some_and(|l| l.width() == 0);
            if !after_heading && !previous_was_blank && !out.is_empty() {
                out.push(Line::raw(""));
            }
            continue;
        }
        after_heading = false;
        out.push(Line::raw(bulleted(raw)));
    }
    while out.last().is_some_and(|l| l.width() == 0) {
        out.pop();
    }
    out
}

/// The text of an ATX heading (`## Added` → `Added`), or `None`.  The closing-`#` form is not
/// worth recognizing in a changelog.
fn heading_text(line: &str) -> Option<&str> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    (1..=6)
        .contains(&hashes)
        .then(|| line[hashes..].strip_prefix(' '))
        .flatten()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// A list item with its marker swapped for [`BULLET`], indentation preserved.
fn bulleted(line: &str) -> String {
    let indent = line.len() - line.trim_start().len();
    let rest = &line[indent..];
    match rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix("* "))
        .or_else(|| rest.strip_prefix("+ "))
    {
        Some(item) => format!("{}{BULLET} {item}", &line[..indent]),
        None => line.to_owned(),
    }
}

/// The installed / latest pair, in that order, so the comparison reads downward.
fn version_rows(theme: &Theme, installed: &str, latest: &str) -> [Line<'static>; 2] {
    [
        version_row(
            theme,
            "Installed version:",
            format!("v{installed}"),
            VERSION_LABEL_WIDTH,
        ),
        version_row(
            theme,
            "Latest release:",
            latest.to_owned(),
            VERSION_LABEL_WIDTH,
        ),
    ]
}

/// One label/value row, the label padded to `width` — the modal body is left-aligned text, so
/// padding is all the alignment there is.
///
/// `width` is a parameter because the callers align against different things: the update
/// check's pair share [`VERSION_LABEL_WIDTH`], while the post-upgrade notice's lone row has
/// nothing to line up with.
fn version_row(theme: &Theme, label: &str, value: String, width: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<width$}"), theme.text_muted()),
        Span::raw(value),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn checking_shows_the_requested_spinner_frame() {
        let body = text(&body_lines(
            theme(),
            UpdateReport::Checking { spinner_frame: 2 },
            "0.1.0",
        ));
        assert!(body.contains(SPINNER_FRAMES[2]), "{body}");
    }

    #[test]
    fn the_spinner_frame_wraps_rather_than_panicking() {
        let body = text(&body_lines(
            theme(),
            UpdateReport::Checking {
                spinner_frame: SPINNER_FRAMES.len() + 1,
            },
            "0.1.0",
        ));
        assert!(body.contains(SPINNER_FRAMES[1]), "{body}");
    }

    #[test]
    fn up_to_date_states_the_verdict_then_both_versions() {
        let body = text(&body_lines(
            theme(),
            UpdateReport::UpToDate { tag: "v0.1.0" },
            "0.1.0",
        ));
        // The verdict is a sentence; the numbers are rows.
        assert!(body.starts_with("edamame is up to date."), "{body}");
        assert!(body.contains("Installed version: v0.1.0"), "{body}");
        assert!(body.contains("Latest release:    v0.1.0"), "{body}");
    }

    #[test]
    fn the_two_version_rows_align_their_values() {
        // The shorter label is padded; a left-aligned body has no other way to make a column.
        let body = text(&body_lines(
            theme(),
            UpdateReport::UpToDate { tag: "v0.2.0" },
            "0.1.0",
        ));
        let value = |label: &str| {
            body.lines()
                .find(|l| l.starts_with(label))
                .map(|l| l[VERSION_LABEL_WIDTH..].to_owned())
                .expect("row present")
        };
        // Both values start at the label column, so the shorter label was padded to it.
        assert_eq!(value("Installed version:"), "v0.1.0");
        assert_eq!(value("Latest release:"), "v0.2.0");
    }

    #[test]
    fn inconclusive_never_claims_a_verdict_it_did_not_reach() {
        let body = text(&body_lines(
            theme(),
            UpdateReport::Inconclusive { tag: "v0.2.0-rc1" },
            "0.1.0",
        ));
        assert!(body.starts_with("Couldn't compare versions."));
        // Both numbers are still shown.
        assert!(body.contains("Installed version:"), "{body}");
        assert!(body.contains("v0.1.0"), "{body}");
        assert!(body.contains("v0.2.0-rc1"), "{body}");
        // And it must not assert the thing it cannot know.
        assert!(!body.contains("up to date"));
        assert!(!body.contains("Update available"));
    }

    #[test]
    fn failed_says_so_without_blaming_the_installed_version() {
        let body = text(&body_lines(theme(), UpdateReport::Failed, "0.1.0"));
        assert!(body.contains("Couldn't check for updates"), "{body}");
        assert!(body.contains("unchanged"), "{body}");
    }

    #[test]
    fn available_lists_the_notes_under_the_versions() {
        let notes = vec![
            "### Added".to_owned(),
            "- **bold** stays literal".to_owned(),
        ];
        let body = text(&body_lines(
            theme(),
            UpdateReport::Available {
                tag: "v0.2.0",
                notes: &notes,
            },
            "0.1.0",
        ));
        assert!(body.starts_with("Update available."), "{body}");
        assert!(body.contains("Installed version: v0.1.0"), "{body}");
        assert!(body.contains("Latest release:    v0.2.0"), "{body}");
        // Structural styling only…
        assert!(body.contains("\nAdded"), "{body}");
        // …but nothing *inside* a line is interpreted.
        assert!(body.contains("• **bold** stays literal"), "{body}");
    }

    #[test]
    fn a_heading_is_bolded_and_a_list_marker_becomes_a_bullet() {
        let notes = vec!["## Fixed".to_owned(), "* a bug".to_owned()];
        let lines = note_lines(&notes);
        assert_eq!(text(&lines), "Fixed\n• a bug");
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD),
            "the heading carries the only styling this function applies"
        );
        assert_eq!(
            lines[1].spans[0].style,
            Style::default(),
            "a list item is plain text with a swapped marker"
        );
    }

    #[test]
    fn the_blank_under_a_heading_goes_but_the_one_above_it_stays() {
        // Keep a Changelog's shape; the blank under a heading separates nothing.
        let notes =
            ["### Added", "", "- one", "", "### Changed", "", "- two", ""].map(str::to_owned);
        assert_eq!(text(&note_lines(&notes)), "Added\n• one\n\nChanged\n• two");
    }

    #[test]
    fn nesting_and_ordinary_prose_are_left_alone() {
        let notes = [
            "Some prose - with a dash.",
            "- top",
            "  - nested",
            "1. numbered",
            "#not-a-heading",
        ]
        .map(str::to_owned);
        assert_eq!(
            text(&note_lines(&notes)),
            "Some prose - with a dash.\n• top\n  • nested\n1. numbered\n#not-a-heading"
        );
    }

    #[test]
    fn available_without_notes_omits_the_notes_block() {
        let body = body_lines(
            theme(),
            UpdateReport::Available {
                tag: "v0.2.0",
                notes: &[],
            },
            "0.1.0",
        );
        assert_eq!(
            body.len(),
            4,
            "headline, blank, and the two version rows — nothing else"
        );
        assert!(!text(&body).ends_with('\n'));
    }

    #[test]
    fn the_upgrade_notice_announces_the_new_version_and_lists_the_notes() {
        let notes = vec!["### Added".to_owned(), "- a thing".to_owned()];
        let body = post_upgrade_body_lines(
            theme(),
            PostUpgradeOccasion::Upgrade,
            PostUpgradeReport::Found { notes: &notes },
            "0.1.2",
        );
        let rendered = text(&body);
        assert!(rendered.starts_with("Updated to v0.1.2."));
        // The headline already names the version, so the labeled row would repeat it.
        assert!(!rendered.contains(VERSION_LABEL));
        assert!(!rendered.contains("Installed version:"));
        // Structural styling, shared with the release-check path.
        assert!(rendered.contains("Added"));
        assert!(!rendered.contains("### Added"));
        assert!(rendered.contains("\u{2022} a thing"));
    }

    #[test]
    fn the_on_demand_opening_names_the_build_without_claiming_an_upgrade() {
        // From the About page, where nothing was just installed: a labeled row, no verdict.
        let notes = vec!["- a thing".to_owned()];
        let body = post_upgrade_body_lines(
            theme(),
            PostUpgradeOccasion::OnDemand,
            PostUpgradeReport::Found { notes: &notes },
            "0.1.2",
        );
        let rendered = text(&body);
        assert!(rendered.starts_with("edamame version: v0.1.2"));
        assert!(!rendered.contains("Updated to"));
        assert!(rendered.contains("\u{2022} a thing"));
    }

    #[test]
    fn a_post_upgrade_report_shows_no_second_version_row() {
        // Nothing to compare against, so "Latest release:" would have no value to show.
        for occasion in [PostUpgradeOccasion::Upgrade, PostUpgradeOccasion::OnDemand] {
            let body = post_upgrade_body_lines(
                theme(),
                occasion,
                PostUpgradeReport::Found { notes: &[] },
                "0.1.2",
            );
            assert!(!text(&body).contains("Latest release:"));
            assert_eq!(
                body.len(),
                1,
                "an empty section leaves just the opening line — nothing else"
            );
        }
    }

    #[test]
    fn a_missing_section_says_so_without_claiming_an_update() {
        // Only the on-demand path reaches this: it must answer without announcing an upgrade
        // that may not have happened.
        let body = post_upgrade_body_lines(
            theme(),
            PostUpgradeOccasion::OnDemand,
            PostUpgradeReport::NotFound,
            "0.1.2",
        );
        let rendered = text(&body);
        assert!(rendered.contains("edamame version: v0.1.2"));
        assert!(rendered.contains("No release notes are bundled"));
        assert!(!rendered.contains("Updated to"));
    }
}
