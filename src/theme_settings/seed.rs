//! The theme settings' seed model is `horizon-theme`'s, shared with the
//! Blitz shell. What is here turns it to and from this shell's own color
//! types.

use alacritty_terminal::vte::ansi::Rgb;
use gpui::Hsla;

pub(crate) use horizon_theme::seed::*;

use crate::theme;

/// `Rgb` (three `u8` channels) -> packed `0xRRGGBB`, matching every other
/// packed-color representation this module and `theme.rs` share. `pub(crate)`
/// so the view (`super::mod`) can pack `terminal_color_scheme()`'s `Rgb`
/// fields into a [`ResolvedFallback`].
pub(crate) fn pack_rgb(rgb: Rgb) -> u32 {
    ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32
}

/// Packed `0xRRGGBB` -> `Hsla`, the exact inverse of `theme::packed_from_hsla`
/// -- used by the view (`super::mod`) to seed each `ColorPickerState`'s
/// `default_value` from this module's packed `u32` representation.
pub(crate) fn u32_to_hsla(value: u32) -> Hsla {
    theme::to_hsla([(value >> 16) as u8, (value >> 8) as u8, value as u8])
}
