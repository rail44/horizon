//! The terminal's view of the app-wide font.
//!
//! The family stack and the live size are app-wide and live in
//! [`crate::ui::font`]: the terminal grid, the agent transcript and the board
//! all draw from the same size, and the `Increase/Decrease/Reset Font Size`
//! commands move it for all of them. What stays here is the terminal-only
//! measurements: the cell's line height, and the row's one baseline.

use gpui::{FontId, Pixels, WindowTextSystem};

pub(super) use crate::ui::font::{font_size, resolved_font};

pub(super) fn line_height() -> f32 {
    // `[terminal] line_height` was retired in the 2026-07-18
    // config-narrowing wave (see AGENTS.md's "Configuration" section):
    // line height is always this fixed formula over the live font size,
    // no file override.
    (font_size() * 18.0 / 13.0).round()
}

/// The vertical extent every piece of text in one grid row is drawn against:
/// the *primary* font's ascent and descent at the live font size. Both are
/// positive distances from the baseline, matching `gpui::LineLayout`.
///
/// A grid row is a row of cells, not a line of prose, so its baseline has to
/// be a property of the cell -- not of whatever characters happen to be in it
/// this frame. That distinction is exactly what a mixed-script row loses by
/// default: `gpui`'s `ShapedLine::paint` centres a line's font box inside
/// `line_height` (`padding_top = (line_height - ascent - descent) / 2`), and
/// a shaped line's `ascent`/`descent` are the *largest* of the fonts it
/// actually used. A row's ASCII span therefore shapes under the primary font
/// while its Japanese span falls back to a CJK font whose ascent is much
/// larger (Noto Sans CJK: 1.16 em against Iosevka's 0.965 em), and the two
/// spans land on different baselines -- the CJK one lower by
/// `(ascent_delta - descent_delta) / 2`, i.e. ~1.25 px at 13 px.
///
/// Every text item of a row is therefore shaped with this one pair
/// substituted for its own, which puts them all back on one baseline without
/// depending on how `gpui` computes the offset (see `shaping::shape_row_items`
/// and `paint::paint_row_items`). The trade-off is deliberate and recorded in
/// `docs/terminal-row-metrics-design.md`: a larger-ascent fallback font's em
/// box then extends above its cell, which is invisible in practice because
/// glyph ink is smaller than the em box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RowMetrics {
    pub(super) ascent: Pixels,
    pub(super) descent: Pixels,
}

/// Reads the primary font's row metrics. `font_id` must be the id
/// `text_system` resolves [`resolved_font`] to -- the same one the cell width
/// is measured from, so the cell, the baseline and the glyph advances all
/// describe one font.
///
/// Reads live metrics rather than caching them here: they are a function of
/// font and size only, and `WindowTextSystem` already memoizes them per
/// `FontId` (its `font_metrics` map), so this is a hash lookup per paint. A
/// size change (the font-size commands) is picked up on the next paint, and
/// the shape cache drops every row at the same time through
/// `CacheEpoch::font_size`.
pub(super) fn row_metrics(
    text_system: &WindowTextSystem,
    font_id: FontId,
    font_size: Pixels,
) -> RowMetrics {
    RowMetrics {
        ascent: text_system.ascent(font_id, font_size),
        descent: text_system.descent(font_id, font_size),
    }
}

#[cfg(test)]
mod tests {
    use super::{resolved_font, row_metrics, RowMetrics};
    use crate::terminal::test_support::{
        text_system, FALLBACK_ASCENT_EM, PRIMARY_ASCENT_EM, PRIMARY_DESCENT_EM,
    };
    use gpui::px;

    #[test]
    fn row_metrics_come_from_the_primary_font_not_the_fallback() {
        let text_system = text_system();
        let font_size = px(13.0);
        let primary = text_system.resolve_font(&resolved_font());

        let metrics = row_metrics(&text_system, primary, font_size);
        assert_eq!(
            metrics,
            RowMetrics {
                ascent: px(13.0 * PRIMARY_ASCENT_EM),
                descent: px(13.0 * PRIMARY_DESCENT_EM),
            }
        );
    }

    /// The discriminating half: a shaped Japanese line really does report a
    /// taller ascent than the row's canonical one, so a `row_metrics` that
    /// read the wrong font (or a line's own metrics) would fail the test
    /// above.
    #[test]
    fn a_japanese_line_reports_a_taller_ascent_than_the_row_metrics() {
        let text_system = text_system();
        let font_size = px(13.0);
        let primary = text_system.resolve_font(&resolved_font());
        let run = gpui::TextRun {
            len: "あ".len(),
            font: resolved_font(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let japanese = text_system.layout_line("あ", font_size, std::slice::from_ref(&run), None);
        assert_eq!(japanese.ascent, px(13.0 * FALLBACK_ASCENT_EM));
        assert!(japanese.ascent > row_metrics(&text_system, primary, font_size).ascent);
    }

    #[test]
    fn row_metrics_track_the_font_size() {
        let text_system = text_system();
        let primary = text_system.resolve_font(&resolved_font());
        let small = row_metrics(&text_system, primary, px(13.0));
        let large = row_metrics(&text_system, primary, px(26.0));
        assert_eq!(large.ascent, small.ascent * 2.0);
        assert_eq!(large.descent, small.descent * 2.0);
    }
}
