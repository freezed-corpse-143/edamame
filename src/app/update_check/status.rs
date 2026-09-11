//! Release-check domain types.
//!
//! The up-to-date / update-available split is decided once, in
//! [`ReleaseStatus::from_fetch`], never re-derived at render time: the update modal and
//! [`super::policy::notice_due`] must agree, and a second display-time comparison is
//! exactly the drift this collapses.  An uncomparable pair gets its own
//! [`ReleaseStatus::Inconclusive`] state — silent like `UpToDate` on the notice path, but
//! the explicit modal must not claim "up to date" above two disagreeing version rows.

use std::cmp::Ordering;

/// The version this binary was built as, with no leading `v`.
pub(crate) const INSTALLED_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A resolved release: its tag plus release notes, already truncated, control-stripped and
/// line-capped by [`super::parse::sanitize_notes`] on the worker thread — the main thread
/// never sees unbounded remote text.  Empty `notes` is an ordinary state, not a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReleaseInfo {
    pub tag: String,
    pub notes: Vec<String>,
}

/// Outcome of a release check, as every consumer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReleaseStatus {
    /// A check is in flight and nothing is cached yet.
    Pending,
    /// The latest release is not newer than [`INSTALLED_VERSION`].
    UpToDate { tag: String },
    /// The latest release is newer than the installed build.
    Available(ReleaseInfo),
    /// Got a tag, but the two versions could not be ordered (pre-release or build suffix,
    /// or not version-shaped).  Silent on the notice path like `UpToDate`, but the modal
    /// shows both numbers rather than claiming a comparison it didn't make.
    Inconclusive { tag: String },
    /// Network error, HTTP error, or an unparseable body.  Reported on an explicit check;
    /// silently dropped by the startup path.
    Failed,
}

impl ReleaseStatus {
    /// The only place `Ok` is split into `Available` / `UpToDate` / `Inconclusive`.
    pub(crate) fn from_fetch(result: Result<ReleaseInfo, String>) -> Self {
        match result {
            Ok(info) => match compare_to_installed(&info.tag) {
                Some(Ordering::Greater) => ReleaseStatus::Available(info),
                Some(_) => ReleaseStatus::UpToDate { tag: info.tag },
                None => ReleaseStatus::Inconclusive { tag: info.tag },
            },
            Err(_) => ReleaseStatus::Failed,
        }
    }

    /// The release tag this status is about, or `None` for `Pending` / `Failed`.  The
    /// single accessor, so consumers don't each grow a match arm per variant.
    pub(crate) fn tag(&self) -> Option<&str> {
        match self {
            ReleaseStatus::UpToDate { tag } | ReleaseStatus::Inconclusive { tag } => Some(tag),
            ReleaseStatus::Available(info) => Some(&info.tag),
            ReleaseStatus::Pending | ReleaseStatus::Failed => None,
        }
    }
}

/// [`compare_versions`] against [`INSTALLED_VERSION`] — the form every non-test caller wants.
pub(crate) fn compare_to_installed(tag: &str) -> Option<Ordering> {
    compare_versions(INSTALLED_VERSION, tag)
}

/// Order `tag` against `installed`, both tolerating a leading `v`.  `Greater` means `tag`
/// names a strictly newer release; a build *ahead* of the latest release compares `Less`.
///
/// `None` means uncomparable — either side unparseable as a dotted numeric version
/// (`v0.2.0-rc1`, `v0.2.0+build`, `nightly`).  Deliberately not folded into "not newer".
pub(crate) fn compare_versions(installed: &str, tag: &str) -> Option<Ordering> {
    let installed = parse_version(installed)?;
    let tag = parse_version(tag)?;
    Some(tag.cmp(&installed))
}

/// Parse `v1.2.3` / `1.2.3` into numeric segments.  `Vec` ordering is lexicographic over
/// them, so a longer tuple sharing a prefix sorts newer (`1.0` < `1.0.1`).
fn parse_version(v: &str) -> Option<Vec<u64>> {
    v.trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(tag: &str) -> ReleaseInfo {
        ReleaseInfo {
            tag: tag.to_owned(),
            notes: Vec::new(),
        }
    }

    fn newer(installed: &str, tag: &str) -> bool {
        compare_versions(installed, tag) == Some(Ordering::Greater)
    }

    #[test]
    fn compare_versions_orders_numeric_versions() {
        assert_eq!(compare_versions("0.1.0", "v0.1.0"), Some(Ordering::Equal));
        assert!(newer("0.1.0", "v0.2.0"));
        assert!(newer("0.1.0", "v0.1.1"));
        assert_eq!(
            compare_versions("0.2.0", "v0.1.9"),
            Some(Ordering::Less),
            "ahead of release is not an update"
        );
        assert!(newer("0.9.0", "v0.10.0"));
        assert!(newer("1.0", "v1.0.1"));
    }

    #[test]
    fn compare_versions_reports_an_uncomparable_pair_as_none() {
        assert_eq!(compare_versions("0.1.0", "nightly"), None);
        assert_eq!(compare_versions("0.1.0", "v0.2.0-rc1"), None);
        assert_eq!(compare_versions("0.1.0-beta", "v0.2.0"), None);
        assert_eq!(compare_versions("0.1.0", "v0.2.0+build.7"), None);
    }

    #[test]
    fn from_fetch_classifies_each_outcome() {
        assert_eq!(
            ReleaseStatus::from_fetch(Err("offline".to_owned())),
            ReleaseStatus::Failed
        );
        let same = ReleaseStatus::from_fetch(Ok(info(INSTALLED_VERSION)));
        assert_eq!(
            same,
            ReleaseStatus::UpToDate {
                tag: INSTALLED_VERSION.to_owned()
            }
        );
        assert_eq!(
            ReleaseStatus::from_fetch(Ok(info("v999.0.0"))),
            ReleaseStatus::Available(info("v999.0.0"))
        );
        assert_eq!(
            ReleaseStatus::from_fetch(Ok(info("v999.0.0-rc1"))),
            ReleaseStatus::Inconclusive {
                tag: "v999.0.0-rc1".to_owned()
            }
        );
    }
}
