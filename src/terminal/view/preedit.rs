use floem::{
    peniko::Color,
    text::{Attrs, AttrsList, TextLayout},
};
use unicode_width::UnicodeWidthStr;

use super::metrics::terminal_font_family;
use super::{font_size, line_height};

pub(super) struct PreeditLayout {
    pub(super) text: TextLayout,
    pub(super) columns: usize,
}

pub(super) fn build_preedit_layout(text: Option<&str>) -> Option<PreeditLayout> {
    let text = text.filter(|text| !text.is_empty())?;
    let family = terminal_font_family();
    let attrs = Attrs::new()
        .color(Color::from_rgb8(233, 236, 242))
        .family(&family)
        .font_size(font_size())
        .line_height(floem::text::LineHeightValue::Px(line_height() as f32));
    let mut layout = TextLayout::new();
    layout.set_text(text, AttrsList::new(attrs), None);
    Some(PreeditLayout {
        text: layout,
        columns: UnicodeWidthStr::width(text),
    })
}
