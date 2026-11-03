//! A fake `PlatformTextSystem` for the terminal's shaping/metrics tests.
//!
//! The real one (cosmic-text, via `gpui-pre-wgpu`) needs installed fonts and
//! a window; these tests only need the *shape* of its behaviour, which is
//! what the row-baseline fix depends on:
//!
//! - two fonts whose ascents disagree, the way a monospace primary and a CJK
//!   fallback do (Iosevka: 0.965 em, Noto Sans CJK: 1.16 em);
//! - a shaped line reporting the largest ascent of the fonts it actually
//!   used, which is how cosmic-text computes `max_ascent` per line.
//!
//! A line with non-ASCII text therefore comes back with the fallback's
//! metrics, exactly as a Japanese run does in the real system.
//!
//! Not linked into the plugin build: `crate::terminal` is native-only.

use gpui::{
    point, px, Font, FontId, FontMetrics, FontRun, GlyphId, LineLayout, Pixels, PlatformTextSystem,
    Point, RenderGlyphParams, ShapedGlyph, ShapedRun, Size, TextRenderingMode, WindowTextSystem,
};
use std::sync::Arc;

/// Font units per em shared by both fake fonts (a 1000-unit em, as in CFF).
const UPEM: f32 = 1000.0;

/// The monospace primary: ascent 0.965 em, descent 0.285 em.
pub(super) const PRIMARY_ASCENT_EM: f32 = 0.965;
pub(super) const PRIMARY_DESCENT_EM: f32 = 0.285;
/// The CJK-ish fallback: a much taller ascent, close to Noto Sans CJK's.
pub(super) const FALLBACK_ASCENT_EM: f32 = 1.16;
pub(super) const FALLBACK_DESCENT_EM: f32 = 0.288;

pub(super) const PRIMARY: FontId = FontId(1);
pub(super) const FALLBACK: FontId = FontId(2);

struct TwoFonts;

fn metrics_of(font_id: FontId) -> FontMetrics {
    let (ascent, descent) = if font_id == FALLBACK {
        (FALLBACK_ASCENT_EM, FALLBACK_DESCENT_EM)
    } else {
        (PRIMARY_ASCENT_EM, PRIMARY_DESCENT_EM)
    };
    FontMetrics {
        units_per_em: UPEM as u32,
        ascent: ascent * UPEM,
        descent: descent * UPEM,
        line_gap: 0.0,
        underline_position: -100.0,
        underline_thickness: 50.0,
        cap_height: 700.0,
        x_height: 500.0,
        bounding_box: gpui::Bounds {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: 500.0,
                height: 1000.0,
            },
        },
    }
}

impl PlatformTextSystem for TwoFonts {
    fn add_fonts(&self, _fonts: Vec<std::borrow::Cow<'static, [u8]>>) -> anyhow::Result<()> {
        Ok(())
    }

    fn all_font_names(&self) -> Vec<String> {
        Vec::new()
    }

    fn font_id(&self, descriptor: &Font) -> anyhow::Result<FontId> {
        Ok(if descriptor.family == "fallback" {
            FALLBACK
        } else {
            PRIMARY
        })
    }

    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        metrics_of(font_id)
    }

    fn typographic_bounds(
        &self,
        _font_id: FontId,
        _glyph_id: GlyphId,
    ) -> anyhow::Result<gpui::Bounds<f32>> {
        Ok(gpui::Bounds {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: 500.0,
                height: 700.0,
            },
        })
    }

    fn advance(&self, _font_id: FontId, _glyph_id: GlyphId) -> anyhow::Result<Size<f32>> {
        Ok(gpui::size(500.0, 0.0))
    }

    fn glyph_for_char(&self, _font_id: FontId, ch: char) -> Option<GlyphId> {
        Some(GlyphId(ch as u32))
    }

    fn glyph_raster_bounds(
        &self,
        _params: &RenderGlyphParams,
    ) -> anyhow::Result<gpui::Bounds<gpui::DevicePixels>> {
        Ok(gpui::Bounds {
            origin: Point {
                x: gpui::DevicePixels(0),
                y: gpui::DevicePixels(0),
            },
            size: Size {
                width: gpui::DevicePixels(6),
                height: gpui::DevicePixels(10),
            },
        })
    }

    fn rasterize_glyph(
        &self,
        _params: &RenderGlyphParams,
        raster_bounds: gpui::Bounds<gpui::DevicePixels>,
    ) -> anyhow::Result<(Size<gpui::DevicePixels>, Vec<u8>)> {
        let count = (raster_bounds.size.width.0 * raster_bounds.size.height.0).max(0) as usize;
        Ok((raster_bounds.size, vec![0u8; count]))
    }

    /// Non-ASCII text reports the fallback font's metrics -- the simulated
    /// form of cosmic-text's per-line `max_ascent` over the fonts it used,
    /// and the thing that makes a mixed-script row's runs disagree.
    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        let font_id = runs.first().map(|run| run.font_id).unwrap_or(PRIMARY);
        let metrics = if text.is_ascii() {
            metrics_of(font_id)
        } else {
            metrics_of(FALLBACK)
        };
        LineLayout {
            font_size,
            width: font_size * text.chars().count() as f32 * 0.5,
            ascent: font_size * (metrics.ascent / UPEM),
            descent: font_size * (metrics.descent / UPEM),
            runs: vec![ShapedRun {
                font_id,
                glyphs: text
                    .char_indices()
                    .enumerate()
                    .map(|(i, (index, ch))| ShapedGlyph {
                        id: GlyphId(ch as u32),
                        position: point(font_size * 0.5 * i as f32, px(0.0)),
                        index,
                        is_emoji: false,
                    })
                    .collect(),
            }],
            len: text.len(),
        }
    }

    fn recommended_rendering_mode(
        &self,
        _font_id: FontId,
        _font_size: Pixels,
    ) -> TextRenderingMode {
        TextRenderingMode::Grayscale
    }
}

/// A text system backed by the two fake fonts.
pub(super) fn text_system() -> WindowTextSystem {
    WindowTextSystem::new(Arc::new(gpui::TextSystem::new(Arc::new(TwoFonts))))
}
