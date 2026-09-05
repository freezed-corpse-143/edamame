//! GitHub latest-release check.  Two triggers share one session cache
//! (`App::latest_release`): the silent startup check (at most once per
//! [`policy::CHECK_INTERVAL_SECS`], only when enabled, only raising a modal once the stack
//! is clear) and the explicit About / palette request, which always re-fetches and also
//! reports "up to date" and failures.  See `docs/dev/update-check.md`.
//!
//! Trust boundaries: [`fetch`] alone touches the network, [`parse`] alone touches remote
//! bytes (bounding them first), [`policy`] is pure, [`status`] is the vocabulary `app`
//! speaks.  `ui` never sees these types.

pub(crate) mod fetch;
pub(crate) mod parse;
pub(crate) mod policy;
pub(crate) mod status;

pub(crate) use fetch::{release_url, spawn_release_check, GITHUB_URL};
pub(crate) use policy::{network_check_due, notice_due, now_unix};
pub(crate) use status::{ReleaseInfo, ReleaseStatus, INSTALLED_VERSION};
