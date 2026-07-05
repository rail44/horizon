use crate::terminal::{TerminalFrame, TerminalLine};
use floem::{
    peniko::Color,
    text::{Attrs, AttrsList, FamilyOwned, TextLayout},
};
use unicode_width::UnicodeWidthChar;

use super::metrics::terminal_font_family;
use super::{font_size, line_height};

pub(super) struct CellLayout {
    pub(super) text: TextLayout,
    pub(super) columns: usize,
    pub(super) fg: [u8; 3],
    pub(super) bg: [u8; 3],
    pub(super) block: Option<BlockElement>,
    pub(super) visible: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BlockElement {
    Full,
    UpperFraction(u8),
    LowerFraction(u8),
    LeftFraction(u8),
    RightFraction(u8),
    Quadrants {
        upper_left: bool,
        upper_right: bool,
        lower_left: bool,
        lower_right: bool,
    },
}

pub(super) fn build_line_layouts(frame: &TerminalFrame) -> Vec<Vec<CellLayout>> {
    let mut lines = Vec::new();
    update_line_layouts(&mut lines, &[], &frame.lines);
    lines
}

/// Row retention + partial rebuild: reuse a row's existing `CellLayout`s
/// (and thus its shaped `TextLayout`s) when the row's content is unchanged
/// since the last frame, and only rebuild rows whose `TerminalLine` differs.
///
/// `TerminalLine` already derives `PartialEq`, so this is a cheap
/// cell-equality diff at the view rather than plumbing
/// `alacritty_terminal`'s damage tracking through the snapshot/session
/// boundary — the smaller of the two options for the same result, since it
/// touches only this module instead of the core/session/contract layers.
pub(super) fn update_line_layouts(
    lines: &mut Vec<Vec<CellLayout>>,
    old: &[TerminalLine],
    new: &[TerminalLine],
) {
    let family = terminal_font_family();

    for (row, new_line) in new.iter().enumerate() {
        let unchanged = row < lines.len() && old.get(row) == Some(new_line);
        if unchanged {
            continue;
        }

        let mut cells = Vec::new();
        for span in &new_line.spans {
            cells.extend(build_span_cells(span, &family));
        }
        if row < lines.len() {
            lines[row] = cells;
        } else {
            lines.push(cells);
        }
    }

    lines.truncate(new.len());
}

pub(super) fn build_span_cells(
    span: &crate::terminal::TerminalSpan,
    family: &[FamilyOwned],
) -> Vec<CellLayout> {
    if span.text.is_empty() {
        return (0..span.columns)
            .map(|_| empty_cell(span.bg))
            .collect::<Vec<_>>();
    }

    let mut cells = Vec::new();
    let mut current = String::new();
    let mut current_columns = 0_usize;

    for ch in span.text.chars() {
        let columns = char_columns(ch);
        if columns == 0 {
            if current.is_empty() {
                current.push(ch);
                current_columns = 1;
            } else {
                current.push(ch);
            }
            continue;
        }

        if !current.is_empty() {
            cells.push(text_cell(
                std::mem::take(&mut current),
                current_columns,
                span.fg,
                span.bg,
                family,
            ));
        }
        current.push(ch);
        current_columns = columns;
    }

    if !current.is_empty() {
        cells.push(text_cell(
            std::mem::take(&mut current),
            current_columns,
            span.fg,
            span.bg,
            family,
        ));
    }

    let used_columns = cells.iter().map(|cell| cell.columns).sum::<usize>();
    if used_columns < span.columns {
        cells.extend((used_columns..span.columns).map(|_| empty_cell(span.bg)));
    }

    cells
}

fn text_cell(
    text: String,
    columns: usize,
    fg: [u8; 3],
    bg: [u8; 3],
    family: &[FamilyOwned],
) -> CellLayout {
    let attrs = Attrs::new()
        .color(Color::from_rgb8(fg[0], fg[1], fg[2]))
        .family(family)
        .font_size(font_size())
        .line_height(floem::text::LineHeightValue::Px(line_height() as f32));
    let mut layout = TextLayout::new();
    layout.set_text(&text, AttrsList::new(attrs), None);
    let block = block_element(text.as_str());
    CellLayout {
        text: layout,
        columns,
        fg,
        bg,
        block,
        visible: true,
    }
}

fn empty_cell(bg: [u8; 3]) -> CellLayout {
    CellLayout {
        text: TextLayout::new(),
        columns: 1,
        fg: [0, 0, 0],
        bg,
        block: None,
        visible: false,
    }
}

fn char_columns(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

fn block_element(text: &str) -> Option<BlockElement> {
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }

    match ch {
        '█' => Some(BlockElement::Full),
        '▔' => Some(BlockElement::UpperFraction(1)),
        '▀' => Some(BlockElement::UpperFraction(4)),
        '▁' => Some(BlockElement::LowerFraction(1)),
        '▂' => Some(BlockElement::LowerFraction(2)),
        '▃' => Some(BlockElement::LowerFraction(3)),
        '▄' => Some(BlockElement::LowerFraction(4)),
        '▅' => Some(BlockElement::LowerFraction(5)),
        '▆' => Some(BlockElement::LowerFraction(6)),
        '▇' => Some(BlockElement::LowerFraction(7)),
        '▏' => Some(BlockElement::LeftFraction(1)),
        '▎' => Some(BlockElement::LeftFraction(2)),
        '▍' => Some(BlockElement::LeftFraction(3)),
        '▌' => Some(BlockElement::LeftFraction(4)),
        '▋' => Some(BlockElement::LeftFraction(5)),
        '▊' => Some(BlockElement::LeftFraction(6)),
        '▉' => Some(BlockElement::LeftFraction(7)),
        '▐' => Some(BlockElement::RightFraction(4)),
        '▕' => Some(BlockElement::RightFraction(1)),
        '▖' => Some(BlockElement::Quadrants {
            upper_left: false,
            upper_right: false,
            lower_left: true,
            lower_right: false,
        }),
        '▗' => Some(BlockElement::Quadrants {
            upper_left: false,
            upper_right: false,
            lower_left: false,
            lower_right: true,
        }),
        '▘' => Some(BlockElement::Quadrants {
            upper_left: true,
            upper_right: false,
            lower_left: false,
            lower_right: false,
        }),
        '▝' => Some(BlockElement::Quadrants {
            upper_left: false,
            upper_right: true,
            lower_left: false,
            lower_right: false,
        }),
        '▚' => Some(BlockElement::Quadrants {
            upper_left: true,
            upper_right: false,
            lower_left: false,
            lower_right: true,
        }),
        '▞' => Some(BlockElement::Quadrants {
            upper_left: false,
            upper_right: true,
            lower_left: true,
            lower_right: false,
        }),
        '▙' => Some(BlockElement::Quadrants {
            upper_left: true,
            upper_right: false,
            lower_left: true,
            lower_right: true,
        }),
        '▛' => Some(BlockElement::Quadrants {
            upper_left: true,
            upper_right: true,
            lower_left: true,
            lower_right: false,
        }),
        '▜' => Some(BlockElement::Quadrants {
            upper_left: true,
            upper_right: true,
            lower_left: false,
            lower_right: true,
        }),
        '▟' => Some(BlockElement::Quadrants {
            upper_left: false,
            upper_right: true,
            lower_left: true,
            lower_right: true,
        }),
        _ => None,
    }
}
