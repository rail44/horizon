use crate::ui::fonts::HORIZON_FONT_FAMILY;
use floem::{
    peniko::Color,
    text::{Attrs, AttrsList, FamilyOwned, TextLayout},
};

use super::{FALLBACK_CELL_WIDTH, FONT_SIZE, LINE_HEIGHT};

#[derive(Clone, Copy, Debug)]
pub(super) struct TerminalMetrics {
    pub(super) cell_width: f64,
    pub(super) line_height: f64,
}

impl Default for TerminalMetrics {
    fn default() -> Self {
        Self {
            cell_width: measured_cell_width(),
            line_height: LINE_HEIGHT,
        }
    }
}

pub(super) fn measured_cell_width() -> f64 {
    let sample = "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm";
    let family = terminal_font_family();
    let attrs = Attrs::new()
        .color(Color::from_rgb8(233, 236, 242))
        .family(&family)
        .font_size(FONT_SIZE)
        .line_height(floem::text::LineHeightValue::Px(LINE_HEIGHT as f32));
    let mut layout = TextLayout::new();
    layout.set_text(sample, AttrsList::new(attrs), None);
    let width = layout.size().width / sample.len() as f64;

    if width.is_finite() && width > 1.0 {
        width
    } else {
        FALLBACK_CELL_WIDTH
    }
}

pub(super) fn terminal_font_family() -> Vec<FamilyOwned> {
    FamilyOwned::parse_list(HORIZON_FONT_FAMILY).collect()
}
