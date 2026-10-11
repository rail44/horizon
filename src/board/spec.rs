//! The measurements only the board's two views use: the reading measure,
//! the list's and the thread's widths and heights, and the task header
//! band. The type scale and rhythm they share with every other view are
//! [`crate::ui::spec`].
//!
//! Lengths are designed at `ui::spec`'s reference body size and scaled from
//! the live font size with [`scaled`]; cell and row counts are constants.

use gpui::Pixels;

use crate::ui::spec::scaled;

// ---------------------------------------------------------------------------
// The reading measure
// ---------------------------------------------------------------------------

/// A monospace cell as a fraction of the em: half, which is also half the
/// advance of a full-width Japanese glyph, so one such glyph is two cells.
pub(crate) const CELL_EM: f32 = 0.5;

/// The line width a post's fold decision is estimated at: 72 cells, i.e.
/// 36 full-width glyphs. Text is drawn across the pane's width; this only
/// feeds the rendered-line count behind the fold threshold.
pub(crate) const MEASURE_CELLS: f32 = 72.0;

/// The estimation width for an owner reply, which is indented and so has
/// less room than a report.
pub(crate) const OWNER_MEASURE_CELLS: f32 = 60.0;

// ---------------------------------------------------------------------------
// Widths and heights
// ---------------------------------------------------------------------------

/// The list view never narrows past this; below it a row's two lines stop
/// holding a title and its meta line.
pub(crate) fn list_min_width() -> Pixels {
    scaled(280.0)
}

/// A list row, two lines tall.
pub(crate) fn row_height() -> Pixels {
    scaled(48.0)
}

/// The strip on a row's left the unread dot sits in.
pub(crate) fn unread_gutter() -> Pixels {
    scaled(8.0)
}

/// How far one level of the task tree indents a row.
pub(crate) fn row_indent() -> Pixels {
    scaled(16.0)
}

/// The fixed cell a row's disclosure triangle sits in, so a leaf's title
/// starts where a parent's does.
pub(crate) fn disclosure() -> Pixels {
    scaled(12.0)
}

/// How wide the status field in a task header band is.
pub(crate) fn status_field() -> Pixels {
    scaled(112.0)
}

/// The dot that marks an unread row.
pub(crate) fn unread_dot() -> Pixels {
    scaled(6.0)
}

/// The dot a row's status collapses to when it is not a chip.
pub(crate) fn status_dot() -> Pixels {
    scaled(8.0)
}

/// The gutter between the pane edge and the body column. The posts carry
/// padding of their own, which the gutter sits outside of.
pub(crate) fn thread_gutter() -> Pixels {
    scaled(24.0)
}

/// How far an owner reply is indented.
pub(crate) fn owner_indent() -> Pixels {
    scaled(24.0)
}

/// The one filled action in a task header.
pub(crate) fn primary_height() -> Pixels {
    scaled(28.0)
}

/// The composer's row bounds.
pub(crate) const COMPOSER_MIN_ROWS: usize = 2;
pub(crate) const COMPOSER_MAX_ROWS: usize = 8;

// ---------------------------------------------------------------------------
// The task header band
// ---------------------------------------------------------------------------

/// How many lines the task title wraps to before it is ellipsized.
pub(crate) const TITLE_MAX_LINES: usize = 2;

/// How narrow the title is allowed to get while it still shares its row
/// with the actions. Below this the band stacks instead.
pub(crate) const HEADER_TITLE_MIN_CELLS: f32 = 24.0;

/// A header action button's horizontal padding, both sides together. Its
/// label is set at the body size, so the whole button is that plus the
/// label's own cells.
pub(crate) fn action_pad() -> Pixels {
    scaled(24.0)
}

#[cfg(test)]
mod tests {
    use super::MEASURE_CELLS;
    use crate::board::model::display_width;

    #[test]
    fn the_fold_estimate_counts_thirty_six_full_width_glyphs_per_line() {
        assert_eq!(display_width(&"あ".repeat(36)) as f32, MEASURE_CELLS);
    }
}
