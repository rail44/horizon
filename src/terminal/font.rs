//! The terminal's view of the app-wide font.
//!
//! The family stack and the live size are app-wide and live in
//! [`crate::ui::font`]: the terminal grid, the agent transcript and the board
//! all draw from the same size, and the `Increase/Decrease/Reset Font Size`
//! commands move it for all of them. What stays here is the one
//! terminal-only measurement, the cell's line height.

pub(super) use crate::ui::font::{font_size, resolved_font};

pub(super) fn line_height() -> f32 {
    // `[terminal] line_height` was retired in the 2026-07-18
    // config-narrowing wave (see AGENTS.md's "Configuration" section):
    // line height is always this fixed formula over the live font size,
    // no file override.
    (font_size() * 18.0 / 13.0).round()
}
