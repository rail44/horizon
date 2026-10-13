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

/// A small action button: 「承認」 and 「拒否」.
pub(crate) fn button_height() -> Pixels {
    scaled(28.0)
}

/// The status dot of a call row.
pub(crate) fn call_dot() -> Pixels {
    scaled(8.0)
}

/// The cell before a call row's title or a diff line's text: the status
/// dot, or the `+`/`−` mark.
pub(crate) fn lead_cell() -> Pixels {
    scaled(16.0)
}

/// The composer's row bounds.
pub(crate) const COMPOSER_MIN_ROWS: usize = 1;
pub(crate) const COMPOSER_MAX_ROWS: usize = 8;

/// One cycle of the thinking dots.
pub(crate) const THINKING_CYCLE_MS: u64 = 1200;
