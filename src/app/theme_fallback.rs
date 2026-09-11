//! Indexed-color theme substitution, shared by [`crate::app::App::new`] and the
//! external-editor reload in [`crate::app::external_editor`].  Sharing matters: `Config::load`
//! returns the on-disk theme every time, so a reload that didn't re-apply would repaint the
//! session in the unreadable palette.  See [`crate::app::modal::ThemeDowngradeModal`] and
//! `Config::theme_downgraded_from`.

use crate::config::theme::indexed_fallback_theme;
use crate::config::{Config, ThemeFile};
use crate::terminal::{Capabilities, ColorDepth};

/// A substitution that fired: the user's `configured` theme and the indexed-color
/// built-in `substituted` for it.
pub(super) struct Downgrade {
    pub theme_file: ThemeFile,
    pub configured: String,
    pub substituted: &'static str,
}

/// Swap `config.theme` for an indexed-color built-in when `caps` lacks 24-bit color.
/// `None` when no swap is needed: truecolor, no color at all, or a theme already in
/// `theme::INDEXED_SAFE_THEMES` (which keeps the swap idempotent across reloads).  The
/// caller derives both the swap and the modal from this one `Option`.
pub(super) fn apply(config: &mut Config, caps: &Capabilities) -> Option<Downgrade> {
    // `NoColor` is handled by `monochrome` in `Theme::from_file`; a swap would change
    // nothing visible, so the modal would be noise.
    if caps.full_color() || caps.color_depth == ColorDepth::NoColor {
        return None;
    }
    let substituted = indexed_fallback_theme(&config.theme, config.appearance)?;
    // Built-in names never hit disk, so the `truecolor` argument is moot.
    let (theme_file, _) = Config::load_theme(substituted, false);
    let configured = config.theme.clone();
    config.theme_downgraded_from = Some(configured.clone());
    config.theme = substituted.to_owned();
    Some(Downgrade {
        theme_file,
        configured,
        substituted,
    })
}
