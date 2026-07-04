use crate::ui::fonts::font_family;
use floem::{
    peniko::Color,
    text::{Attrs, AttrsList, FamilyOwned, TextLayout},
};

use super::{font_size, line_height, FALLBACK_CELL_WIDTH};

#[derive(Clone, Copy, Debug)]
pub(super) struct TerminalMetrics {
    pub(super) cell_width: f64,
    pub(super) line_height: f64,
}

impl Default for TerminalMetrics {
    fn default() -> Self {
        Self {
            cell_width: measured_cell_width(),
            line_height: line_height(),
        }
    }
}

pub(super) fn measured_cell_width() -> f64 {
    let sample = "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm";
    let family = terminal_font_family();
    let attrs = Attrs::new()
        .color(Color::from_rgb8(233, 236, 242))
        .family(&family)
        .font_size(font_size())
        .line_height(floem::text::LineHeightValue::Px(line_height() as f32));
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
    FamilyOwned::parse_list(font_family()).collect()
}
