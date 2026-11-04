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
/// the *primary* font's ascent and descent at the live font size, in the sign
/// convention `gpui::LineLayout` uses: both are positive distances from the
/// baseline. `WindowTextSystem::descent` reports the *font's* convention,
/// which is negative (see [`row_metrics`]), so the two differ by a sign.
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
///
/// The descent is negated because the two systems disagree about its sign, and
/// these metrics are substituted into a `LineLayout`:
///
/// - `gpui::FontMetrics` (what `WindowTextSystem::descent` reads) carries the
///   descent *negated*: `gpui-pre-wgpu` builds it from swash as
///   `descent: -metrics.descent`, and swash already reports the positive
///   value (`-hhea.descender`).
/// - `gpui::LineLayout` carries it positive: cosmic-text's `max_descent` is a
///   `max` over `glyph.descent`, which it computes without the negation.
///
/// `LineLayout::paint` places both the glyph baseline *and* the underline from
/// that pair -- `padding_top = (line_height - ascent - descent) / 2`, baseline
/// `= padding_top + ascent`, underline `= baseline + descent * 0.618`. Feeding
/// it a negative descent therefore halves the wrong sum and lifts the
/// underline above the baseline, into the glyph bodies, which is exactly the
/// reported terminal bug. See
/// `row_metrics_use_the_layout_descent_convention`'s test.
pub(super) fn row_metrics(
    text_system: &WindowTextSystem,
    font_id: FontId,
    font_size: Pixels,
) -> RowMetrics {
    RowMetrics {
        ascent: text_system.ascent(font_id, font_size),
        descent: -text_system.descent(font_id, font_size),
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

    /// The regression this exists for: `FontMetrics` and `LineLayout` carry the
    /// descent with opposite signs, and `RowMetrics` is substituted into a
    /// `LineLayout`. Reading `WindowTextSystem::descent` straight through fed a
    /// negative descent into `paint`, which halves the wrong sum for
    /// `padding_top` and lifts the underline above the baseline -- a line drawn
    /// through the glyphs instead of under them.
    ///
    /// Comparing against a *native* layout descent is the structural form of
    /// the check: it pins the convention to the thing the value replaces,
    /// rather than to a constant that could drift.
    #[test]
    fn row_metrics_use_the_layout_descent_convention() {
        let text_system = text_system();
        let font_size = px(13.0);
        let primary = text_system.resolve_font(&resolved_font());
        let run = gpui::TextRun {
            len: "abc".len(),
            font: resolved_font(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let native = text_system.layout_line("abc", font_size, std::slice::from_ref(&run), None);
        let metrics = row_metrics(&text_system, primary, font_size);

        assert_eq!(metrics.descent, native.descent);
        assert_eq!(metrics.ascent, native.ascent);
        assert!(
            metrics.descent > px(0.0),
            "a descent below the baseline is positive in a LineLayout"
        );
    }

    /// Where that pair puts the row's baseline and underline, in the exact
    /// terms `gpui`'s `LineLayout::paint` uses. The underline has to clear the
    /// baseline (glyph bodies sit on it) and stay inside the cell.
    #[test]
    fn the_row_baseline_and_underline_land_correctly_in_a_cell() {
        let text_system = text_system();
        let primary = text_system.resolve_font(&resolved_font());
        let metrics = row_metrics(&text_system, primary, px(13.0));
        let line_height = px(18.0);
        let thickness = px(1.0);

        // `paint_line`: padding_top = (line_height - ascent - descent) / 2.
        let padding_top = (line_height - metrics.ascent - metrics.descent) / 2.0;
        let baseline = padding_top + metrics.ascent;
        // `underline_y_offset`: baseline + descent * 0.618 -- the prose factor
        // gpui uses; what matters here is only that it is added, not subtracted.
        let underline = baseline + metrics.descent * 0.618;

        assert_eq!(padding_top, px(0.875), "the row is centred on the font box");
        assert_eq!(baseline, px(13.42), "Iosevka's baseline in an 18 px row");
        assert!(underline > baseline, "the underline sits under the glyphs");
        assert!(underline + thickness <= line_height, "and inside the cell");
    }
}
