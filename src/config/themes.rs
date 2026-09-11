//! Built-in palette constructors: one submodule per shipped theme, each a
//! `pub fn palette() -> Palette` referenced directly from
//! [`super::theme::BUILTIN_THEMES`].  A new theme needs a module here plus a
//! `BUILTIN_THEMES` entry in `theme.rs`.

pub mod util;

pub mod dark_256;
pub mod light_256;
pub mod monochrome_dark;

pub mod ayu;
pub mod catppuccin;
pub mod catppuccin_latte;
pub mod dracula;
pub mod edamame;
pub mod everforest;
pub mod github_dark;
pub mod github_light;
pub mod gruvbox;
pub mod gruvbox_light;
pub mod kanagawa;
pub mod monokai;
pub mod nord;
pub mod one_dark;
pub mod orng;
pub mod rainbow;
pub mod rose_pine;
pub mod rose_pine_dawn;
pub mod solarized_dark;
pub mod solarized_light;
pub mod synthwave84;
pub mod tokyo_night;
pub mod tokyo_night_day;
pub mod zenburn;
