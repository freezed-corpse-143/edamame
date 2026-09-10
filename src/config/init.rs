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
    use std::collections::HashMap;

    use super::*;
    use crate::config::Config;

    /// Machine-managed bookkeeping that [`Config::save`] writes on its own.  Deliberately absent
    /// from the shipped template — a fresh config should not carry state the user never sets — so
    /// [`reference_config_documents_every_default_setting`] both skips these in its coverage sweep
    /// and asserts they never appear as reference lines.
    const BOOKKEEPING_KEYS: &[&str] = &[
        "editor.seen_terminal_fingerprints",
        "editor.last_update_check",
        "editor.update_notified_for",
        "editor.last_version_seen",
    ];

    /// Drop a trailing ` # comment` from a value, leaving quoted `#`s alone.
    fn strip_inline_comment(value: &str) -> &str {
        let mut in_string = false;
        for (i, c) in value.char_indices() {
            match c {
                '"' => in_string = !in_string,
                '#' if !in_string => return &value[..i],
                _ => {}
            }
        }
        value
    }

    /// Collect `section.key -> value` for every scalar assignment in a TOML-ish string, tracking
    /// the current `[section]` header.  A leading `# ` (a commented-out reference line) and any
    /// trailing inline comment are ignored, so the annotated template and a bare
    /// `toml::to_string_pretty` serialization parse through the same lens.  First write per key
    /// wins.  Non-identifier keys (prose lines that happen to contain `=`) are skipped.
    fn scalar_assignments(src: &str) -> HashMap<String, String> {
        let mut section = String::new();
        let mut out = HashMap::new();
        for raw in src.lines() {
            let line = raw.trim_start();
            let line = line.strip_prefix('#').map_or(line, str::trim_start);
            if let Some(rest) = line.strip_prefix('[') {
                if let Some(name) = rest.split(']').next() {
                    section = name.trim().to_string();
                }
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                continue;
            }
            let value = strip_inline_comment(value.trim()).trim().to_string();
            let full = if section.is_empty() {
                key.to_string()
            } else {
                format!("{section}.{key}")
            };
            out.entry(full).or_insert(value);
        }
        out
    }

    /// The shipped `config.toml` must list every configurable setting exactly once, at its
    /// compiled-in default — so adding a `Config` field without documenting it, or letting a
    /// default drift from the comment beside it, is a test failure rather than a silent gap.
    /// Machine-written bookkeeping is the sole, asserted, exception.
    #[test]
    fn reference_config_documents_every_default_setting() {
        let serialized = toml::to_string_pretty(&Config::default()).expect("serialize default");
        let defaults = scalar_assignments(&serialized);
        let reference = scalar_assignments(REFERENCE_CONFIG_TOML);

        for (key, default_value) in &defaults {
            // Empty arrays (`export.custom = []`) have no scalar reference line; the custom-export
            // block is shown as a commented `[[export.custom]]` example instead.
            if default_value == "[]" || BOOKKEEPING_KEYS.contains(&key.as_str()) {
                continue;
            }
            match reference.get(key) {
                Some(reference_value) => assert_eq!(
                    reference_value, default_value,
                    "config.toml lists `{key} = {reference_value}` but the default is \
                     `{default_value}` — update the reference line",
                ),
                None => panic!(
                    "config.toml has no line for `{key}` (default `{default_value}`) — document \
                     it, or add it to BOOKKEEPING_KEYS if edamame writes it automatically",
                ),
            }
        }

        for key in BOOKKEEPING_KEYS {
            assert!(
                !reference.contains_key(*key),
                "`{key}` is machine-written bookkeeping and must not ship in config.toml",
            );
        }
    }

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
