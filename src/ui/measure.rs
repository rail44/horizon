//! A text's width and height in cells, estimated without laying it out:
//! one glyph is one cell, two for the full-width ranges Japanese is written
//! in. A view decides whether to fold a text on these.

/// How many cells `ch` occupies: two for the full-width ranges Japanese is
/// written in, one otherwise. The ranges are the East Asian Wide and
/// Fullwidth blocks.
pub(crate) fn cell_width(ch: char) -> usize {
    let code = ch as u32;
    let wide = matches!(code,
        0x1100..=0x115F      // Hangul Jamo
        | 0x2E80..=0x303E    // CJK radicals, kangxi, CJK punctuation
        | 0x3041..=0x33FF    // kana, bopomofo, compatibility
        | 0x3400..=0x4DBF    // CJK extension A
        | 0x4E00..=0x9FFF    // CJK unified
        | 0xA000..=0xA4CF    // Yi
        | 0xAC00..=0xD7A3    // Hangul syllables
        | 0xF900..=0xFAFF    // CJK compatibility ideographs
        | 0xFE10..=0xFE19
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60    // fullwidth forms
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F  // emoji
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD  // CJK extensions B and beyond
    );
    1 + usize::from(wide)
}

/// How many cells `line` occupies.
pub(crate) fn display_width(line: &str) -> usize {
    line.chars().map(cell_width).sum()
}

/// How many rows `text` takes in a column `cells` wide: each of its own
/// lines wraps as often as its width needs, and an empty line still takes a
/// row. A word-wrap boundary can move a character to the next row, so this
/// is the length the fold decision is made on rather than a promise about
/// the laid-out text.
pub(crate) fn rendered_lines(text: &str, cells: usize) -> usize {
    let cells = cells.max(1);
    text.lines()
        .map(|line| display_width(line).div_ceil(cells).max(1))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::{cell_width, display_width, rendered_lines};

    #[test]
    fn a_japanese_glyph_is_two_cells_and_latin_is_one() {
        assert_eq!(cell_width('あ'), 2);
        assert_eq!(cell_width('録'), 2);
        assert_eq!(cell_width('、'), 2);
        assert_eq!(cell_width('a'), 1);
        assert_eq!(cell_width(' '), 1);
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("あい"), 4);
        assert_eq!(display_width("あa"), 3);
    }

    #[test]
    fn a_line_wraps_as_often_as_the_column_needs() {
        assert_eq!(rendered_lines("", 72), 0, "no lines at all");
        assert_eq!(rendered_lines("short", 72), 1);
        // 72 cells is 36 full-width glyphs, so 37 of them take two rows.
        assert_eq!(rendered_lines(&"あ".repeat(36), 72), 1);
        assert_eq!(rendered_lines(&"あ".repeat(37), 72), 2);
        // An empty line between paragraphs still takes a row.
        assert_eq!(rendered_lines("a\n\nb", 72), 3);
    }
}
