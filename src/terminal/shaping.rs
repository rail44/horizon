//! Builds cacheable terminal row items; painting and cache invalidation
//! stay with the view and `shape_cache` respectively.

use gpui::{
    px, Font, FontStyle, Hsla, Pixels, StrikethroughStyle, TextRun, UnderlineStyle,
    WindowTextSystem,
};

use super::{char_columns, glyphs, shape_cache::RowItem};
use crate::theme;

/// Builds one row's text layer as cache-ready [`RowItem`]s: walks the
/// row's spans, classifies characters, and shapes the ordinary-text runs.
/// Pure with respect to the window — everything here depends only on row
/// content, the current font metrics, and resolved colors (the
/// cache's epoch axes), which is what makes the result safe to replay on
/// later frames.
pub(super) fn shape_row_items(
    line: &horizon_terminal_core::TerminalLine,
    palette_overrides: &[(u16, [u8; 3])],
    font: &Font,
    font_size: Pixels,
    cell_width: Pixels,
    text_system: &WindowTextSystem,
) -> Vec<RowItem> {
    let mut items = Vec::new();
    let mut col = 0_usize;
    for span in &line.spans {
        let span_start = col;
        col += span.columns;
        if span.text.trim().is_empty() {
            continue;
        }
        let fg = theme::to_hsla(theme::resolve(span.fg, palette_overrides));

        // Box-drawing/block/sextant/Braille characters (see `glyphs`)
        // paint as geometry sized exactly to their cell instead of
        // shaped font glyphs, which can't fill a cell taller than the
        // font's own em box -- the seams reported in ASCII-art
        // rectangles. A span's characters are walked one at a time so a
        // run mixing geometric and ordinary text (same fg/bg, so
        // already merged into one span) still gets both treatments;
        // consecutive ordinary-text characters are still batched
        // through one `shape_line` call, matching the old whole-span
        // behavior when a span has no geometric characters at all.
        let chars: Vec<char> = span.text.chars().collect();
        let mut char_index = 0;
        let mut col_in_span = 0_usize;
        while char_index < chars.len() {
            let ch = chars[char_index];
            if glyphs::is_geometric(ch) {
                let width_cols = char_columns(ch).max(1);
                items.push(RowItem::Glyph {
                    col: span_start + col_in_span,
                    ch,
                    width_cols,
                    fg,
                });
                col_in_span += width_cols;
                char_index += 1;
                continue;
            }

            let run_start_col = col_in_span;
            let mut run_text = String::new();
            while char_index < chars.len() && !glyphs::is_geometric(chars[char_index]) {
                run_text.push(chars[char_index]);
                col_in_span += char_columns(chars[char_index]).max(1);
                char_index += 1;
            }
            let run_columns = col_in_span - run_start_col;
            let run_chars = run_text.chars().count();
            // Snap glyphs to the cell grid only when every char in the
            // run occupies the same number of columns; a mixed-width
            // run keeps natural shaping (positioned correctly at its
            // start column, with only intra-run drift possible).
            let force_width = if run_columns == run_chars {
                Some(cell_width)
            } else if run_columns == run_chars * 2 {
                Some(cell_width * 2.0)
            } else {
                None
            };
            let run = text_run(span, run_text.len(), font, fg, palette_overrides);
            let shaped = text_system.shape_line(run_text.into(), font_size, &[run], force_width);
            items.push(RowItem::Text {
                col: span_start + run_start_col,
                shaped: Box::new(shaped),
            });
        }
    }
    items
}

/// Resolve decorations once for an ordinary text run. Geometry has its
/// own paint path and deliberately carries no text decorations.
fn text_run(
    span: &horizon_terminal_core::TerminalSpan,
    byte_len: usize,
    font: &Font,
    fg: Hsla,
    palette_overrides: &[(u16, [u8; 3])],
) -> TextRun {
    // v7 style attributes. Underline color falls back to the
    // run's own fg (the SGR 58 contract on
    // `TerminalSpan::underline_color`); Curl maps to gpui's
    // wavy underline, while Dotted/Dashed draw as a plain
    // single line for now (gpui's `UnderlineStyle` has no
    // dotted/dashed variant). Geometric characters take
    // the `paint_glyph` path and carry no style attributes --
    // box-drawing cells are their own geometry.
    let underline = match span.underline {
        horizon_terminal_core::TerminalUnderline::None => None,
        kind => {
            let color = span
                .underline_color
                .map(|color| theme::to_hsla(theme::resolve(color, palette_overrides)))
                .unwrap_or(fg);
            Some(UnderlineStyle {
                // Approximate a double underline by thickness
                // -- gpui draws exactly one line per run.
                thickness: match kind {
                    horizon_terminal_core::TerminalUnderline::Double => px(2.0),
                    _ => px(1.0),
                },
                color: Some(color),
                wavy: kind == horizon_terminal_core::TerminalUnderline::Curl,
            })
        }
    };
    // An OSC 8 hyperlink renders underlined even on unstyled text so
    // links are discoverable without hover state; an explicit SGR 4
    // style wins (its color and wavy shape are preserved).
    let underline = underline.or_else(|| {
        span.url.as_ref().map(|_| UnderlineStyle {
            thickness: px(1.0),
            color: Some(fg),
            wavy: false,
        })
    });
    let strikethrough = span.strikethrough.then(|| StrikethroughStyle {
        thickness: px(1.0),
        color: Some(fg),
    });
    let run_font = if span.italic {
        Font {
            style: FontStyle::Italic,
            ..font.clone()
        }
    } else {
        font.clone()
    };
    TextRun {
        len: byte_len,
        font: run_font,
        color: fg,
        background_color: None,
        underline,
        strikethrough,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_terminal_core::{TerminalFrame, TerminalUnderline};

    #[test]
    fn hyperlink_decoration_preserves_explicit_underline_style() {
        let mut span = TerminalFrame::from_text("link".into()).lines[0].spans[0].clone();
        span.url = Some("https://example.com".into());
        span.underline = TerminalUnderline::Curl;
        span.italic = true;
        span.strikethrough = true;
        let color = gpui::rgb(0x123456).into();
        let run = text_run(&span, 4, &gpui::font("monospace"), color, &[]);
        let underline = run.underline.unwrap();
        assert!(underline.wavy);
        assert_eq!(underline.color, Some(color));
        assert_eq!(run.font.style, FontStyle::Italic);
        assert_eq!(run.strikethrough.unwrap().color, Some(color));
        assert_eq!(run.len, 4);
    }

    #[test]
    fn a_link_gets_an_underline_without_changing_plain_text() {
        let mut span = TerminalFrame::from_text("aé".into()).lines[0].spans[0].clone();
        let color = gpui::rgb(0x123456).into();
        let font = gpui::font("monospace");
        let plain = text_run(&span, span.text.len(), &font, color, &[]);
        assert!(plain.underline.is_none());
        assert_eq!(plain.len, 3);
        span.url = Some("https://example.com".into());
        let linked = text_run(&span, span.text.len(), &font, color, &[]);
        let underline = linked.underline.unwrap();
        assert!(!underline.wavy);
        assert_eq!(underline.thickness, px(1.0));
        assert_eq!(underline.color, Some(color));
    }
}
