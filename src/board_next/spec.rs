//! The measurements both board views are drawn to: the type scale, the
//! vertical rhythm, the reading measure, and the fixed widths.
//!
//! The thread view and the list view are two panes of the same workspace, so
//! every number they have in common lives here rather than in either of
//! them.

use gpui::{px, FontWeight, Pixels};

// ---------------------------------------------------------------------------
// The type scale: five steps, two weights
// ---------------------------------------------------------------------------

/// One step of the scale: a size and the line height that goes with it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Step {
    pub(crate) size: Pixels,
    pub(crate) line_height: Pixels,
}

/// The task title.
pub(crate) const T1: Step = Step {
    size: px(17.0),
    line_height: px(26.0),
};

/// A heading inside a post.
pub(crate) const T2: Step = Step {
    size: px(15.0),
    line_height: px(24.0),
};

/// Running text, and a list row's title.
pub(crate) const BODY: Step = Step {
    size: px(14.0),
    line_height: px(26.0),
};

/// Times, counters, and secondary labels.
pub(crate) const META: Step = Step {
    size: px(12.0),
    line_height: px(18.0),
};

/// Chips and the smallest counters.
pub(crate) const MICRO: Step = Step {
    size: px(11.0),
    line_height: px(16.0),
};

/// Everything that is not the one anchor of its unit.
pub(crate) const REGULAR: FontWeight = FontWeight::NORMAL;

/// The single 600-weight anchor a unit is allowed: a post's author, a task's
/// title, a row's title.
pub(crate) const ANCHOR: FontWeight = FontWeight::SEMIBOLD;

// ---------------------------------------------------------------------------
// Vertical rhythm
// ---------------------------------------------------------------------------

/// A label to its value, an icon to its text.
pub(crate) const GAP_LABEL: Pixels = px(4.0);

/// A header line to the body under it; one paragraph to the next.
pub(crate) const GAP_TIGHT: Pixels = px(8.0);

/// A container's own vertical padding.
pub(crate) const PAD_Y: Pixels = px(12.0);

/// A container's own horizontal padding.
pub(crate) const PAD_X: Pixels = px(16.0);

/// One post to the next; the space around a code block.
pub(crate) const GAP_UNIT: Pixels = px(16.0);

/// Before an in-post heading; the task header to the thread.
pub(crate) const GAP_SECTION: Pixels = px(24.0);

/// The thread to the composer.
pub(crate) const GAP_COMPOSER: Pixels = px(32.0);

// ---------------------------------------------------------------------------
// The reading measure
// ---------------------------------------------------------------------------

/// A monospace cell as a fraction of the em: half, which is also half the
/// advance of a full-width Japanese glyph, so one such glyph is two cells.
pub(crate) const CELL_EM: f32 = 0.5;

/// What the body column aims at: 72 cells, i.e. 36 full-width glyphs.
pub(crate) const MEASURE_CELLS: f32 = 72.0;

/// The column shrinks with the pane below its measure, but never past this:
/// a narrower line breaks mid-word often enough to stop reading as prose.
pub(crate) const MEASURE_FLOOR_CELLS: f32 = 40.0;

/// An owner reply is narrower than an agent report, so the two speakers
/// differ by container rather than only by name.
pub(crate) const OWNER_MEASURE_CELLS: f32 = 60.0;

/// A code block may run wider than prose and scroll on its own.
pub(crate) const CODE_MEASURE_CELLS: f32 = 110.0;

/// How wide `cells` cells are at `step`'s size.
pub(crate) fn measure(cells: f32, step: Step) -> Pixels {
    step.size * (CELL_EM * cells)
}

/// The same width for a container that adds [`PAD_X`] on both sides, so the
/// text inside it still lands on `cells`.
pub(crate) fn padded_measure(cells: f32, step: Step) -> Pixels {
    measure(cells, step) + PAD_X * 2.0
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// The tint a task header band and an owner reply sit on.
pub(crate) const BAND_TINT: f32 = 0.03;

/// A selected list row's full-bleed tint.
pub(crate) const SELECTION_TINT: f32 = 0.07;

/// A status chip's tinted background.
pub(crate) const CHIP_TINT: f32 = 0.14;

// ---------------------------------------------------------------------------
// Fixed widths and heights
// ---------------------------------------------------------------------------

/// The list view never narrows past this; below it a row's two lines stop
/// holding a title and its meta line.
pub(crate) const LIST_MIN_WIDTH: Pixels = px(280.0);

/// A list row, two lines tall.
pub(crate) const ROW_HEIGHT: Pixels = px(48.0);

/// The strip on a row's left the unread dot sits in.
pub(crate) const UNREAD_GUTTER: Pixels = px(8.0);

/// The dot that marks an unread row.
pub(crate) const UNREAD_DOT: Pixels = px(6.0);

/// The dot a row's status collapses to when it is not a chip.
pub(crate) const STATUS_DOT: Pixels = px(8.0);

/// A selected row's, and the cursored post's, left accent bar.
pub(crate) const ACCENT_BAR: Pixels = px(2.0);

/// The gutter between the pane edge and the body column. The posts carry
/// padding of their own, which the gutter sits outside of.
pub(crate) const THREAD_GUTTER: Pixels = px(24.0);

/// How far an owner reply is indented.
pub(crate) const OWNER_INDENT: Pixels = px(24.0);

/// The one filled action in a task header.
pub(crate) const PRIMARY_HEIGHT: Pixels = px(28.0);

/// A container's corner.
pub(crate) const RADIUS: Pixels = px(4.0);

/// A chip's corner.
pub(crate) const CHIP_RADIUS: Pixels = px(2.0);

/// How tall the fade over a folded post is, and how many solid strips make
/// it: a quad crosses the host boundary with one solid color, so the fade
/// is stacked steps rather than a gradient
/// (`docs/preview-pane-design.md`).
pub(crate) const FADE_HEIGHT: Pixels = px(24.0);
pub(crate) const FADE_STEPS: usize = 4;

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
/// label is set at [`BODY`]'s size, so the whole button is that plus the
/// label's own cells.
pub(crate) const ACTION_PAD: Pixels = px(24.0);

#[cfg(test)]
mod tests {
    use super::{measure, padded_measure, BODY, MEASURE_CELLS, MEASURE_FLOOR_CELLS, PAD_X};
    use crate::board_next::model::display_width;
    use gpui::px;

    #[test]
    fn the_body_column_is_seventy_two_cells_of_body_type() {
        // 14px type, half-em cells: 72 cells is 504px, and the Japanese
        // line it holds is 36 glyphs.
        assert_eq!(measure(MEASURE_CELLS, BODY), px(504.0));
        assert_eq!(display_width(&"あ".repeat(36)) as f32, MEASURE_CELLS);
        assert_eq!(
            padded_measure(MEASURE_CELLS, BODY),
            px(504.0) + PAD_X * 2.0,
            "a padded container still holds the same measure"
        );
    }

    #[test]
    fn the_column_floor_is_narrower_than_the_measure() {
        assert_eq!(measure(MEASURE_FLOOR_CELLS, BODY), px(280.0));
        assert!(measure(MEASURE_FLOOR_CELLS, BODY) < measure(MEASURE_CELLS, BODY));
    }
}
