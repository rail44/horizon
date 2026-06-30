use floem::{
    peniko::Color,
    text::{Attrs, AttrsList, TextLayout},
};
use unicode_width::UnicodeWidthStr;

use super::metrics::terminal_font_family;
use super::{FONT_SIZE, LINE_HEIGHT};

pub(super) struct PreeditLayout {
    pub(super) text: TextLayout,
    pub(super) columns: usize,
}

pub(super) fn build_preedit_layout(text: Option<&str>) -> Option<PreeditLayout> {
    let text = text.filter(|text| !text.is_empty())?;
    let family = terminal_font_family();
    let attrs = Attrs::new()
        .color(Color::rgb8(233, 236, 242))
        .family(&family)
        .font_size(FONT_SIZE)
        .line_height(floem::text::LineHeightValue::Px(LINE_HEIGHT as f32));
    let mut layout = TextLayout::new();
    layout.set_text(text, AttrsList::new(attrs));
    Some(PreeditLayout {
        text: layout,
        columns: UnicodeWidthStr::width(text),
    })
}
