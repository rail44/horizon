//! Box-drawing, block-element, sextant and Braille characters are drawn
//! from shapes, not shaped as font glyphs. The shapes are
//! `horizon-shell-runtime`'s `terminal_glyphs`, shared with the Blitz
//! shell; what is here paints them into a gpui window. Pixel output is not
//! unit-tested (it needs a live `Window`).

use gpui::{point, px, Bounds, Hsla, PathBuilder, Pixels, Point, Size, Window};
use horizon_shell_runtime::terminal_glyphs::{self as shapes, GlyphShape};

pub(crate) use horizon_shell_runtime::terminal_glyphs::is_geometric;

fn at(place: shapes::Point<f32>) -> Point<Pixels> {
    point(px(place.x), px(place.y))
}

/// Paints `ch` geometrically into `cell_bounds` (its already-positioned,
/// already-sized cell rectangle) in `color`. Returns `false` (having
/// painted nothing) if `ch` is not covered -- callers should only reach
/// this after checking [`is_geometric`].
pub(crate) fn paint_glyph(
    window: &mut Window,
    cell_bounds: Bounds<Pixels>,
    ch: char,
    color: Hsla,
    font_size: Pixels,
    scale_factor: f32,
) -> bool {
    let cell = shapes::Bounds {
        origin: shapes::Point {
            x: cell_bounds.origin.x.into(),
            y: cell_bounds.origin.y.into(),
        },
        size: shapes::Size {
            width: cell_bounds.size.width.into(),
            height: cell_bounds.size.height.into(),
        },
    };
    shapes::glyph_shapes(
        cell,
        ch,
        font_size.into(),
        scale_factor,
        &mut |shape| match shape {
            GlyphShape::Fill { bounds, alpha } => {
                let mut fill_color = color;
                fill_color.a *= alpha;
                let bounds = Bounds {
                    origin: at(bounds.origin),
                    size: Size {
                        width: px(bounds.size.width),
                        height: px(bounds.size.height),
                    },
                };
                window.paint_quad(gpui::fill(bounds, fill_color));
            }
            GlyphShape::RoundedCorner(spec) => {
                let mut builder = PathBuilder::stroke(px(spec.stroke_width));
                builder.move_to(at(spec.start));
                builder.line_to(at(spec.curve_start));
                builder.cubic_bezier_to(at(spec.curve_end), at(spec.control_a), at(spec.control_b));
                builder.line_to(at(spec.end));
                if let Ok(path) = builder.build() {
                    window.paint_path(path, color);
                }
            }
            GlyphShape::Diagonal(spec) => {
                let mut builder = PathBuilder::stroke(px(spec.stroke_width));
                builder.move_to(at(spec.start));
                builder.line_to(at(spec.end));
                if let Ok(path) = builder.build() {
                    window.paint_path(path, color);
                }
            }
        },
    )
}
