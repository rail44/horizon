//! A row of the screen as text for Parley: its runs of color, the runs
//! that have a background, and the shaping of it.

use std::borrow::Cow;
use std::cell::RefCell;
use std::ops::Range;

use parley::{FontContext, FontFamily, Layout, LayoutContext, PositionedLayoutItem, StyleProperty};

/// A run's color, as Parley carries it through shaping.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(super) struct Ink(pub(super) [u8; 3]);

/// One row: its text, the color of each run by byte range, and the runs
/// that have a background by column range.
pub(super) struct Row {
    pub(super) text: String,
    pub(super) colors: Vec<(Range<usize>, Ink)>,
    pub(super) backgrounds: Vec<(Range<usize>, [u8; 3])>,
}

thread_local! {
    /// The system's fonts, read once for every terminal: reading them
    /// takes a tenth of a second.
    static FONTS: RefCell<FontContext> = RefCell::new(FontContext::new());
}

pub(super) struct Shaper {
    layouts: LayoutContext<Ink>,
    /// The font stack, as CSS writes one, and the size in CSS pixels.
    family: String,
    size: f32,
}

impl Shaper {
    pub(super) fn with_font(family: &str, size: f32) -> Self {
        Self {
            layouts: LayoutContext::new(),
            family: family.to_string(),
            size,
        }
    }

    pub(super) fn shape(&mut self, row: &Row, scale: f32) -> Layout<Ink> {
        let mut layout = FONTS.with_borrow_mut(|fonts| {
            let mut builder = (self.layouts).ranged_builder(fonts, &row.text, scale, true);
            builder.push_default(FontFamily::Source(Cow::Borrowed(&self.family)));
            builder.push_default(StyleProperty::FontSize(self.size));
            for (range, ink) in &row.colors {
                builder.push(StyleProperty::Brush(*ink), range.clone());
            }
            builder.build(&row.text)
        });
        layout.break_all_lines(None);
        layout
    }

    fn sample(&mut self, text: &str, scale: f32) -> Layout<Ink> {
        let row = Row {
            text: text.to_string(),
            colors: Vec::new(),
            backgrounds: Vec::new(),
        };
        self.shape(&row, scale)
    }

    /// Where a row's baseline is below its top, in device pixels at
    /// `scale`, for a row `line_height` CSS pixels tall: the primary font's
    /// own ascent and descent, centered in the row. A row that falls back
    /// to another font for some characters (Japanese) would otherwise sit
    /// on that font's taller line (the GPUI shell's `src/terminal/font.rs`
    /// does the same).
    pub(super) fn baseline(&mut self, scale: f32, line_height: f64) -> f32 {
        let layout = self.sample("M", scale);
        let run = (layout.lines().flat_map(|line| line.items())).find_map(|item| match item {
            PositionedLayoutItem::GlyphRun(run) => Some(run),
            _ => None,
        });
        let Some(run) = run else {
            return 0.;
        };
        let metrics = run.run().font_metrics();
        let padding = (line_height as f32 * scale - metrics.ascent - metrics.descent) / 2.;
        padding + metrics.ascent
    }

    /// A cell's width in device pixels at `scale`.
    pub(super) fn cell_width(&mut self, scale: f32) -> f64 {
        self.sample(&"M".repeat(32), scale).width() as f64 / 32.
    }
}
