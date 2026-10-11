//! The measurements every view draws to: the type scale, the vertical
//! rhythm, and the surfaces' tints and corners.
//!
//! The table is designed at a reference body size of [`REFERENCE_BODY`] px
//! and scaled from the live font size ([`crate::ui::font::font_size`]), so
//! the font-size setting and commands move it with the terminal. Every
//! scaled value is rounded to a whole pixel. At the default base of 13 the
//! body is 13px and the other steps round from the 14-based ratios: t1
//! 16/24, t2 14/22, body 13/24, meta 11/17, micro 10/15.
//!
//! Corners, hairlines, tints and weights do not zoom: they stay constants.

use gpui::{px, FontWeight, Pixels};

use crate::ui::font::font_size;

/// The body size the table below is designed at.
pub(crate) const REFERENCE_BODY: f32 = 14.0;

/// How far the live font size is from the designed table.
pub(crate) fn scale() -> f32 {
    font_size() / REFERENCE_BODY
}

/// A measurement designed at the reference body size, at the live size.
pub(crate) fn scaled(px_at_reference: f32) -> Pixels {
    scaled_at(px_at_reference, scale())
}

fn scaled_at(px_at_reference: f32, scale: f32) -> Pixels {
    px((px_at_reference * scale).round())
}

// ---------------------------------------------------------------------------
// The type scale: five steps, two weights
// ---------------------------------------------------------------------------

/// One step of the scale: a size and the line height that goes with it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Step {
    pub(crate) size: Pixels,
    pub(crate) line_height: Pixels,
}

impl Step {
    fn at(size: f32, line_height: f32, scale: f32) -> Self {
        Self {
            size: scaled_at(size, scale),
            line_height: scaled_at(line_height, scale),
        }
    }
}

fn t1_at(scale: f32) -> Step {
    Step::at(17.0, 26.0, scale)
}

fn t2_at(scale: f32) -> Step {
    Step::at(15.0, 24.0, scale)
}

fn body_at(scale: f32) -> Step {
    Step::at(REFERENCE_BODY, 26.0, scale)
}

fn meta_at(scale: f32) -> Step {
    Step::at(12.0, 18.0, scale)
}

fn micro_at(scale: f32) -> Step {
    Step::at(11.0, 16.0, scale)
}

/// A title: the task title on the board.
pub(crate) fn t1() -> Step {
    t1_at(scale())
}

/// A heading inside running text.
pub(crate) fn t2() -> Step {
    t2_at(scale())
}

/// Running text, and a list row's title.
pub(crate) fn body() -> Step {
    body_at(scale())
}

/// Times, counters, and secondary labels.
pub(crate) fn meta() -> Step {
    meta_at(scale())
}

/// Chips and the smallest counters.
pub(crate) fn micro() -> Step {
    micro_at(scale())
}

/// Everything that is not the one anchor of its unit.
pub(crate) const REGULAR: FontWeight = FontWeight::NORMAL;

/// The single 600-weight anchor a unit is allowed: a post's author, a task's
/// title, a row's title.
pub(crate) const ANCHOR: FontWeight = FontWeight::SEMIBOLD;

// ---------------------------------------------------------------------------
// Vertical rhythm
// ---------------------------------------------------------------------------

/// A label to its value, an icon to its text.
pub(crate) fn gap_label() -> Pixels {
    scaled(4.0)
}

/// A header line to the body under it; one paragraph to the next.
pub(crate) fn gap_tight() -> Pixels {
    scaled(8.0)
}

/// A container's own vertical padding.
pub(crate) fn pad_y() -> Pixels {
    scaled(12.0)
}

/// A container's own horizontal padding.
pub(crate) fn pad_x() -> Pixels {
    scaled(16.0)
}

/// One unit (a post) to the next; the space around a code block.
pub(crate) fn gap_unit() -> Pixels {
    scaled(16.0)
}

/// Before an in-text heading; a header to the content under it.
pub(crate) fn gap_section() -> Pixels {
    scaled(24.0)
}

/// The content to the composer under it.
pub(crate) fn gap_composer() -> Pixels {
    scaled(32.0)
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// The tint a header band and an owner reply sit on.
pub(crate) const BAND_TINT: f32 = 0.03;

/// A selected row's full-bleed tint.
pub(crate) const SELECTION_TINT: f32 = 0.07;

/// A status chip's tinted background.
pub(crate) const CHIP_TINT: f32 = 0.14;

/// A selected row's, and the cursored unit's, left accent bar.
pub(crate) const ACCENT_BAR: Pixels = px(2.0);

/// A container's corner.
pub(crate) const RADIUS: Pixels = px(4.0);

/// A chip's corner.
pub(crate) const CHIP_RADIUS: Pixels = px(2.0);

/// A view's own floor. A guest window opens at one pixel and is resized
/// afterwards, so without one a view would have to lay out text in no width
/// at all on its first frame.
pub(crate) const VIEW_MIN_WIDTH: Pixels = px(240.0);

/// How tall the fade over folded text is, and how many solid strips make
/// it: a quad crosses the preview host boundary with one solid color, so
/// the fade is stacked steps rather than a gradient
/// (`docs/preview-pane-design.md`).
pub(crate) fn fade_height() -> Pixels {
    scaled(24.0)
}
pub(crate) const FADE_STEPS: usize = 4;

#[cfg(test)]
mod tests {
    use super::{body_at, meta_at, micro_at, scaled_at, t1_at, t2_at, Step, REFERENCE_BODY};
    use gpui::px;

    fn step(size: f32, line_height: f32) -> Step {
        Step {
            size: px(size),
            line_height: px(line_height),
        }
    }

    fn steps(scale: f32) -> [Step; 5] {
        [
            t1_at(scale),
            t2_at(scale),
            body_at(scale),
            meta_at(scale),
            micro_at(scale),
        ]
    }

    const RHYTHM: [f32; 7] = [4.0, 8.0, 12.0, 16.0, 16.0, 24.0, 32.0];

    #[test]
    fn at_the_reference_body_the_steps_are_the_designed_table() {
        assert_eq!(
            steps(1.0),
            [
                step(17.0, 26.0),
                step(15.0, 24.0),
                step(14.0, 26.0),
                step(12.0, 18.0),
                step(11.0, 16.0),
            ]
        );
        for gap in RHYTHM {
            assert_eq!(scaled_at(gap, 1.0), px(gap));
        }
    }

    #[test]
    fn at_twice_the_reference_every_value_doubles() {
        let scale = 28.0 / REFERENCE_BODY;
        for (doubled, designed) in steps(scale).into_iter().zip(steps(1.0)) {
            assert_eq!(doubled.size, designed.size * 2.0);
            assert_eq!(doubled.line_height, designed.line_height * 2.0);
        }
        for gap in RHYTHM {
            assert_eq!(scaled_at(gap, scale), px(gap * 2.0));
        }
    }

    #[test]
    fn at_a_base_of_thirteen_the_steps_round_to_whole_pixels() {
        assert_eq!(
            steps(13.0 / REFERENCE_BODY),
            [
                step(16.0, 24.0),
                step(14.0, 22.0),
                step(13.0, 24.0),
                step(11.0, 17.0),
                step(10.0, 15.0),
            ]
        );
    }
}
