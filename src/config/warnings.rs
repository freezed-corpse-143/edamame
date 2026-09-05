//! Non-fatal warnings produced by config loading.

use std::path::PathBuf;

/// One problem detected while reading a config file.  Surfaced as a startup modal; non-fatal —
/// the loader still returns the best-effort parsed value (or default).
#[derive(Debug, Clone)]
pub struct ConfigWarning {
    /// File the warning came from; display verbatim.
    pub path: PathBuf,
    pub kind: WarningKind,
}

/// What went wrong with a single config file.  Each variant carries the detail the modal renders,
/// keeping the body strings next to the loader rather than in the App.
#[derive(Debug, Clone)]
pub enum WarningKind {
    /// Formatted `toml::de::Error` (already carries line and column); the file falls back to
    /// defaults.
    ParseError(String),
    /// Dotted paths `serde_ignored` reported as unconsumed (e.g. `editor.tab_widht`).  The rest of
    /// the struct still applies, so this warning is the user's only signal.
    UnknownKeys(Vec<String>),
    /// `keybindings.toml` entries naming an unknown action or unparseable key.  Bad entries are
    /// dropped; valid ones still take effect.
    InvalidKeybindings(Vec<String>),
    /// A value parsed but fell outside its supported range; the default was substituted.  `key` is
    /// the dotted TOML path.
    InvalidValue { key: String, message: String },
    /// The active theme was neither a built-in nor a file in `themes/`.  A capability-appropriate
    /// built-in is substituted and `config.toml` rewritten so this doesn't re-surface.
    MissingTheme { requested: String, fallback: String },
}
