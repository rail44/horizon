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

/// The composer's row bounds.
pub(crate) const COMPOSER_MIN_ROWS: usize = 2;
pub(crate) const COMPOSER_MAX_ROWS: usize = 8;

/// One cycle of the thinking dots.
pub(crate) const THINKING_CYCLE_MS: u64 = 1200;

/// An earlier turn whose text estimates to more than this many rendered
/// lines is drawn folded.
pub(crate) const FOLD_LINES: usize = 12;

/// How many lines of a folded turn's text stay visible.
pub(crate) const FOLD_VISIBLE_LINES: usize = 3;

/// The column width, in cells, a turn's text is estimated at.
pub(crate) const MEASURE_CELLS: usize = 72;
