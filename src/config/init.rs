//! First-run scaffolding: write the shipped default config files
//! (`config.toml`, `keybindings.toml`, `export/default.css.example`) into
//! the user's config directory, only when each file is absent.

use std::path::Path;

use super::readers::{INDEXED_FALLBACK_THEME, TRUECOLOR_FALLBACK_THEME};

/// The annotated reference `config.toml` compiled into the binary.
///
/// Seeded on first run by [`ensure_default_files_in`], and reused as the merge base by
/// [`Config::save`](super::config::Config::save) when the user's file is missing at save time —
/// otherwise a save racing a deleted `config.toml` would strip every comment permanently.
pub(super) const REFERENCE_CONFIG_TOML: &str = include_str!("../../config/config.toml");

/// Testable core of [`super::config::Config::ensure_default_files`]: create the config
/// directory and its `themes/` and `export/` subdirectories, then write the shipped defaults
/// if absent.  Never overwrites an existing file.
///
/// Built-in themes are compiled in and resolved before any disk read, so no
/// `themes/<builtin>.toml` is written; the empty directory exists for custom themes.
///
/// `truecolor` selects the seeded `theme`: indexed-color terminals quantize
/// [`TRUECOLOR_FALLBACK_THEME`] badly, so they are seeded with [`INDEXED_FALLBACK_THEME`].
pub(super) fn ensure_default_files_in(dir: &Path, truecolor: bool) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!(error = %e, dir = %dir.display(), "failed to create config dir");
        return;
    }
    let themes_dir = dir.join("themes");
    if let Err(e) = std::fs::create_dir_all(&themes_dir) {
        tracing::warn!(error = %e, dir = %themes_dir.display(), "failed to create themes dir");
        return;
    }

    // A selectable `default.css` is deliberately NOT written: the built-in default is the
    // compiled-in stylesheet, so a copy here would surface a duplicate in the Export HTML
    // picker.  The `.example` seed is excluded by `list_export_stylesheets`'s `.css` filter.
    let export_dir = dir.join("export");
    if let Err(e) = std::fs::create_dir_all(&export_dir) {
        tracing::warn!(error = %e, dir = %export_dir.display(), "failed to create export dir");
        return;
    }

    write_if_absent(&dir.join("config.toml"), &seed_config_toml(truecolor));
    write_if_absent(
        &dir.join("keybindings.toml"),
        include_str!("../../config/keybindings.toml"),
    );
    write_if_absent(
        &export_dir.join("default.css.example"),
        include_str!("../../config/export/default.css"),
    );
}

/// The `config.toml` body to seed on first run: the reference file verbatim, except that a
/// non-truecolor terminal gets its single `theme = "…"` assignment rewritten.  Everything else,
/// comments included, is untouched.
fn seed_config_toml(truecolor: bool) -> String {
    if truecolor {
        return REFERENCE_CONFIG_TOML.to_owned();
    }
    REFERENCE_CONFIG_TOML.replacen(
        &format!("theme = \"{TRUECOLOR_FALLBACK_THEME}\""),
        &format!("theme = \"{INDEXED_FALLBACK_THEME}\""),
        1,
    )
}

fn write_if_absent(path: &Path, contents: &str) {
    if path.exists() {
        return;
    }
    if let Err(e) = std::fs::write(path, contents) {
        tracing::warn!(error = %e, path = %path.display(), "failed to write default file");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_keeps_reference_verbatim_on_truecolor() {
        assert_eq!(seed_config_toml(true), REFERENCE_CONFIG_TOML);
    }

    #[test]
    fn seed_swaps_theme_for_indexed_terminals() {
        // The reference config must keep spelling the default as a plain `theme = "…"`
        // assignment, or the swap would silently no-op.
        assert!(REFERENCE_CONFIG_TOML.contains(&format!("theme = \"{TRUECOLOR_FALLBACK_THEME}\"")));
        let seeded = seed_config_toml(false);
        assert!(seeded.contains(&format!("theme = \"{INDEXED_FALLBACK_THEME}\"")));
        assert!(!seeded.contains(&format!("theme = \"{TRUECOLOR_FALLBACK_THEME}\"")));
    }
}
