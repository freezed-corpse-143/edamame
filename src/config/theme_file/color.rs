use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// A color read from TOML: either a string (`"magenta"`, `"#ff00aa"`, `"236"`) parsed by
/// ratatui's `Color`, or a bare integer treated as a palette index — TOML distinguishes the
/// two, and quoting `236` to mean "palette index 236" would be awkward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ColorField {
    Named(Color),
    Indexed(u8),
}

impl From<ColorField> for Color {
    fn from(c: ColorField) -> Self {
        match c {
            ColorField::Named(c) => c,
            ColorField::Indexed(i) => Self::Indexed(i),
        }
    }
}

impl From<Color> for ColorField {
    fn from(c: Color) -> Self {
        match c {
            Color::Indexed(i) => Self::Indexed(i),
            other => Self::Named(other),
        }
    }
}
