//! Builds cacheable terminal row items; painting and cache invalidation
//! stay with the view and `shape_cache` respectively.

use gpui::{
    px, DecorationRun, Font, FontStyle, Hsla, LineLayout, Pixels, StrikethroughStyle, TextRun,
    UnderlineStyle, WindowTextSystem,
};

use super::{font::RowMetrics, glyphs, paint::char_columns, shape_cache::RowItem};
use crate::theme;

/// One run of ordinary text, shaped and then re-based on the row's canonical
/// metrics: `layout` carries the *row's* ascent/descent instead of the ones
/// the fonts this run actually used would report. That substitution is the
/// whole point -- `LineLayout::paint` centres a line's font box inside
/// `line_height`, so runs that disagree about their metrics land on different
/// baselines. See [`super::font::RowMetrics`].
///
/// The decoration runs travel with the layout because `LineLayout::paint`
/// takes them explicitly where `ShapedLine::paint` reads its own copy: the
/// same one-run decoration `shape_line` used to build internally.
pub(super) struct NormalizedLine {
    pub(super) layout: LineLayout,
    pub(super) decorations: Vec<DecorationRun>,
}

/// Re-bases an already-shaped layout on the row's canonical metrics, keeping
/// its glyphs, width and length. The single place the substitution happens,
/// so the row loop and the IME preedit overlay cannot drift apart.
///
/// Takes `&LineLayout` rather than an owned one because `layout_line` hands
/// back a cache-owned `Arc`: the runs are copied (see the call site).
pub(super) fn normalized_layout(shaped: &LineLayout, metrics: RowMetrics) -> LineLayout {
    LineLayout {
        font_size: shaped.font_size,
        width: shaped.width,
        ascent: metrics.ascent,
        descent: metrics.descent,
        runs: shaped.runs.clone(),
        len: shaped.len,
    }
}

/// Builds one row's text layer as cache-ready [`RowItem`]s: walks the row's
/// spans, classifies characters, and shapes the ordinary-text runs.
/// Pure with respect to the window — everything here depends only on row
/// content, the current font metrics, and resolved colors (the
/// cache's epoch axes), which is what makes the result safe to replay on
/// later frames. `metrics` is the row's canonical baseline: every text item
/// this returns carries exactly that pair.
pub(super) fn shape_row_items(
    line: &horizon_terminal_core::TerminalLine,
    palette_overrides: &[(u16, [u8; 3])],
    font: &Font,
    font_size: Pixels,
    cell_width: Pixels,
    metrics: RowMetrics,
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
        // through one `layout_line` call, matching the old whole-span
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
            // `layout_line` rather than `shape_line`: the same shaping, minus
            // the `ShapedLine` wrapper whose own metrics would re-impose
            // themselves at paint time.
            let shaped = text_system.layout_line(
                &run_text,
                font_size,
                std::slice::from_ref(&run),
                force_width,
            );
            items.push(RowItem::Text {
                col: span_start + run_start_col,
                line: Box::new(NormalizedLine {
                    // The substitution: the row's baseline, not this run's
                    // fonts'. `normalized_layout` copies the shaped runs
                    // because `LineLayout` is not `Clone` and the text
                    // system's cache keeps its own `Arc` alive -- once per
                    // shaped run, after which `shape_cache` replays the
                    // finished item on every later frame.
                    layout: normalized_layout(&shaped, metrics),
                    decorations: vec![DecorationRun {
                        len: run_text.len() as u32,
                        color: run.color,
                        background_color: run.background_color,
                        underline: run.underline,
                        strikethrough: run.strikethrough,
                    }],
                }),
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
    use crate::terminal::font::{resolved_font, row_metrics};
    use crate::terminal::test_support::{text_system, FALLBACK_ASCENT_EM};
    use horizon_terminal_core::{
        NamedColor, TerminalColor, TerminalFrame, TerminalLine, TerminalSpan, TerminalUnderline,
    };

    fn span(text: &str, columns: usize) -> TerminalSpan {
        TerminalSpan {
            text: text.to_string(),
            columns,
            fg: TerminalColor::Named(NamedColor::Foreground),
            bg: TerminalColor::Named(NamedColor::Background),
            italic: false,
            strikethrough: false,
            underline: TerminalUnderline::None,
            underline_color: None,
            url: None,
        }
    }

    /// A mixed-script row the way the frame builder produces one: ASCII and
    /// Japanese end up in separate spans because `push_styled_cell` only
    /// merges neighbouring cells whose column width matches their character
    /// count, which a 2-column CJK char never matches beside 1-column ASCII.
    fn mixed_row() -> TerminalLine {
        TerminalLine {
            spans: vec![span("abc", 3), span("あいう", 6), span("123", 3)],
        }
    }

    /// Shapes a row and returns the items plus the row metrics they should
    /// all carry. The fake text system advances half an em per char, so a
    /// cell width of 9 px makes the grid snap a visible, distinguishable
    /// correction to a 6.5 px natural advance.
    fn shape(line: &TerminalLine) -> (Vec<RowItem>, RowMetrics) {
        let text_system = text_system();
        let font = resolved_font();
        let font_size = px(13.0);
        let metrics = row_metrics(&text_system, text_system.resolve_font(&font), font_size);
        let items = shape_row_items(line, &[], &font, font_size, px(9.0), metrics, &text_system);
        (items, metrics)
    }

    fn text_items(items: &[RowItem]) -> Vec<(usize, &NormalizedLine)> {
        items
            .iter()
            .filter_map(|item| match item {
                RowItem::Text { col, line } => Some((*col, line.as_ref())),
                RowItem::Glyph { .. } => None,
            })
            .collect()
    }

    #[test]
    fn every_text_item_of_a_mixed_row_shares_the_row_metrics() {
        let (items, metrics) = shape(&mixed_row());

        let text_items = text_items(&items);
        assert_eq!(text_items.len(), 3, "one text item per span");
        for (col, item) in &text_items {
            assert_eq!(item.layout.ascent, metrics.ascent, "ascent at col {col}");
            assert_eq!(item.layout.descent, metrics.descent, "descent at col {col}");
        }

        // The items still start at their spans' cell offsets, so the metrics
        // substitution moved nothing horizontally.
        let cols: Vec<usize> = text_items.iter().map(|(col, _)| *col).collect();
        assert_eq!(cols, vec![0, 3, 9]);
    }

    /// The regression this change exists for: an unnormalized Japanese run
    /// carries the fallback font's taller ascent, which is what puts it on a
    /// lower baseline than the ASCII runs beside it.
    #[test]
    fn a_japanese_run_would_otherwise_carry_a_taller_ascent() {
        let text_system = text_system();
        let font = resolved_font();
        let font_size = px(13.0);
        let metrics = row_metrics(&text_system, text_system.resolve_font(&font), font_size);
        let run = text_run(
            &span("あいう", 6),
            "あいう".len(),
            &font,
            gpui::black(),
            &[],
        );
        let unnormalized =
            text_system.layout_line("あいう", font_size, std::slice::from_ref(&run), None);
        assert_eq!(unnormalized.ascent, px(13.0 * FALLBACK_ASCENT_EM));
        assert!(unnormalized.ascent > metrics.ascent);

        // ... and the item we actually cache does not.
        let (items, _) = shape(&mixed_row());
        for (_, item) in text_items(&items) {
            assert_ne!(item.layout.ascent, unnormalized.ascent);
        }
    }

    #[test]
    fn a_geometric_run_keeps_its_cell_geometry_and_shapes_no_text() {
        let (items, _) = shape(&TerminalLine {
            spans: vec![span("─┬─", 3)],
        });
        assert_eq!(items.len(), 3);
        for (index, item) in items.iter().enumerate() {
            match item {
                RowItem::Glyph {
                    col,
                    width_cols,
                    ch,
                    ..
                } => {
                    assert_eq!(*col, index);
                    assert_eq!(*width_cols, 1);
                    assert_eq!(*ch, ['─', '┬', '─'][index]);
                }
                RowItem::Text { .. } => panic!("expected geometry only"),
            }
        }
    }

    /// Grid snapping is what a whole-row `shape_line` would have cost us:
    /// `force_width` is one value per call, so a width-uniform run has to
    /// snap while a mixed-width one must not.
    #[test]
    fn a_width_uniform_run_snaps_to_the_grid_and_a_mixed_one_does_not() {
        let (uniform, _) = shape(&TerminalLine {
            spans: vec![span("abc", 3)],
        });
        let (col, line) = text_items(&uniform)[0];
        assert_eq!(col, 0);
        assert_eq!(
            line.layout.runs[0]
                .glyphs
                .iter()
                .map(|glyph| glyph.position.x)
                .collect::<Vec<_>>(),
            vec![px(0.0), px(9.0), px(18.0)]
        );

        // "aあb" is one span of mixed widths (1 + 2 + 1 columns, 3 chars), so
        // the run keeps the fake's natural half-em advance.
        let (mixed, _) = shape(&TerminalLine {
            spans: vec![span("aあb", 4)],
        });
        let (_, line) = text_items(&mixed)[0];
        assert_eq!(
            line.layout.runs[0]
                .glyphs
                .iter()
                .map(|glyph| glyph.position.x)
                .collect::<Vec<_>>(),
            vec![px(0.0), px(6.5), px(13.0)]
        );
    }

    #[test]
    fn a_text_run_carries_one_decoration_run_matching_its_span() {
        let mut span = span("link", 4);
        span.url = Some("https://example.com".into());
        span.underline = TerminalUnderline::Curl;
        let (items, _) = shape(&TerminalLine { spans: vec![span] });
        let (_, line) = text_items(&items)[0];
        assert_eq!(line.decorations.len(), 1);
        assert_eq!(line.decorations[0].len, 4);
        assert!(line.decorations[0].underline.unwrap().wavy);
    }

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
