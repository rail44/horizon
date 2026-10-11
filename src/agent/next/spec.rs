//! The measurements only the agent pane uses. The type scale and rhythm it
//! shares with every other view are [`crate::ui::spec`]; every length here
//! is designed at that module's reference body size and scaled with it.

use gpui::Pixels;

use crate::ui::spec::scaled;

/// The gutter between the pane edge and the conversation column.
pub(crate) fn thread_gutter() -> Pixels {
    scaled(24.0)
}

/// How far a message sent into the session is indented.
pub(crate) fn user_indent() -> Pixels {
    scaled(24.0)
}

/// A small action button: 「停止」 in the band, 「承認」 and 「拒否」.
pub(crate) fn button_height() -> Pixels {
    scaled(28.0)
}

/// The composer's row bounds.
pub(crate) const COMPOSER_MIN_ROWS: usize = 2;
pub(crate) const COMPOSER_MAX_ROWS: usize = 8;

/// One cycle of the thinking dots.
pub(crate) const THINKING_CYCLE_MS: u64 = 1200;
