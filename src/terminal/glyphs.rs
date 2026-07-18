//! Geometric synthesis for box-drawing (U+2500-257F), block-element
//! (U+2580-259F), Legacy-Computing sextant (U+1FB00-1FB3B), and Braille
//! (U+2800-28FF) characters: `paint_terminal`'s span loop (`super::mod`)
//! calls into this module for any character in these ranges instead of
//! shaping it as a font glyph. A font's em box cannot fill a terminal cell
//! whose height exceeds it (line_height 18px vs. a 13px font), which is
//! exactly the seam reported in ASCII-art rectangles -- see
//! `docs/research/gpui-terminal-presentation-2026-07-18.md`'s "Box-drawing /
//! block elements" finding.
//!
//! Ported from lassejlv/termy's `crates/terminal_ui/src/grid.rs`
//! (<https://github.com/lassejlv/termy>, commit
//! f2fae0b749925dc823255da6831514149f7a00d0, MIT License) -- specifically
//! its pure `char + cell metrics -> geometry` layer (`box_draw_segments`,
//! `block_element_geometry`, the sextant/Braille decoders, and the
//! rounded-corner/diagonal path builders), which that project's own doc
//! comments describe as having zero coupling to its grid/row-cache types.
//! Horizon's own `TerminalGrid`-equivalent paint-batching machinery was not
//! ported; only the geometry.
//!
//! ```text
//! MIT License
//!
//! Copyright (c) 2026 Lasse Vestergaard
//!
//! Permission is hereby granted, free of charge, to any person obtaining a
//! copy of this software and associated documentation files (the
//! "Software"), to deal in the Software without restriction, including
//! without limitation the rights to use, copy, modify, merge, publish,
//! distribute, sublicense, and/or sell copies of the Software, and to
//! permit persons to whom the Software is furnished to do so, subject to
//! the following conditions:
//!
//! The above copyright notice and this permission notice shall be included
//! in all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
//! OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
//! NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
//! LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
//! OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
//! WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
//! ```
//!
//! One piece is *not* ported: the device-pixel stroke-center snapping used
//! by rounded corners (`snap_center_to_device_pixel` below) is an
//! independent reimplementation against `Window::scale_factor()`. termy's
//! own `snapped_stroke_center` rounds in logical-pixel space only (correct
//! at 1x, but not at fractional HiDPI scale factors); the
//! 2026-07-18 survey attributes the scale-aware technique to paneflow
//! (GPL, read-only reference) -- that source was not read, only its
//! documented approach ("snap stroke centers to device pixels against
//! `scale_factor()`") re-derived from first principles here.

use gpui::{point, px, Bounds, Hsla, PathBuilder, Pixels, Point, Size, Window};

const BOX_DRAWING_START: u32 = 0x2500;
const BOX_DRAWING_END: u32 = 0x257F;
const BLOCK_ELEMENTS_START: u32 = 0x2580;
const BLOCK_ELEMENTS_END: u32 = 0x259F;
const SEXTANT_MOSAIC_START: u32 = 0x1FB00;
const SEXTANT_MOSAIC_END: u32 = 0x1FB3B;
const BRAILLE_PATTERNS_START: u32 = 0x2800;
const BRAILLE_PATTERNS_END: u32 = 0x28FF;

const QUAD_UPPER_LEFT: u8 = 0b0001;
const QUAD_UPPER_RIGHT: u8 = 0b0010;
const QUAD_LOWER_LEFT: u8 = 0b0100;
const QUAD_LOWER_RIGHT: u8 = 0b1000;

// ---------------------------------------------------------------------
// Pure geometry: char + cell metrics -> cell-relative rects/paths. No
// `Window` dependency; unit-tested directly below.
// ---------------------------------------------------------------------

/// A single rectangle within a cell, expressed as fractions of the cell's
/// width/height (0.0..1.0), plus an alpha multiplier for shaded blocks
/// (U+2591-2593).
#[derive(Clone, Copy, Debug, PartialEq)]
struct CellRect {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    alpha: f32,
}

impl CellRect {
    const fn new(left: f32, top: f32, right: f32, bottom: f32, alpha: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
            alpha,
        }
    }
}

const EMPTY_CELL_RECT: CellRect = CellRect::new(0.0, 0.0, 0.0, 0.0, 0.0);

/// The set of cell-relative rectangles that compose a single glyph. Fixed
/// capacity (8) to avoid heap allocation -- the most complex box-drawing
/// connector (a double-line cross, U+256C) expands to 8 rects before
/// overlapping collinear runs are merged back together, and no covered
/// glyph needs more.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CellGeometry {
    rects: [CellRect; 8],
    rect_count: usize,
}

impl CellGeometry {
    const fn empty() -> Self {
        Self {
            rects: [EMPTY_CELL_RECT; 8],
            rect_count: 0,
        }
    }

    const fn one(rect: CellRect) -> Self {
        Self {
            rects: [
                rect,
                EMPTY_CELL_RECT,
                EMPTY_CELL_RECT,
                EMPTY_CELL_RECT,
                EMPTY_CELL_RECT,
                EMPTY_CELL_RECT,
                EMPTY_CELL_RECT,
                EMPTY_CELL_RECT,
            ],
            rect_count: 1,
        }
    }

    fn push_rect(&mut self, rect: CellRect) {
        debug_assert!(
            self.rect_count < self.rects.len(),
            "glyph geometry exceeded rect capacity"
        );
        if self.rect_count >= self.rects.len() {
            return;
        }
        self.rects[self.rect_count] = rect;
        self.rect_count += 1;
    }

    fn rects(&self) -> &[CellRect] {
        &self.rects[..self.rect_count]
    }

    /// Merges any pair of rects that share the same axis track and overlap
    /// or touch, so a simple light-cross (one vertical + one horizontal
    /// rect overlapping at center) stays as two rects rather than
    /// fragmenting into four.
    fn merge_collinear_overlaps(&mut self) {
        const EPSILON: f32 = 1e-6;

        let mut i = 0;
        while i < self.rect_count {
            let mut j = i + 1;
            while j < self.rect_count {
                let a = self.rects[i];
                let b = self.rects[j];

                let same_vertical_track = (a.left - b.left).abs() <= EPSILON
                    && (a.right - b.right).abs() <= EPSILON
                    && a.top <= b.bottom + EPSILON
                    && b.top <= a.bottom + EPSILON;
                let same_horizontal_track = (a.top - b.top).abs() <= EPSILON
                    && (a.bottom - b.bottom).abs() <= EPSILON
                    && a.left <= b.right + EPSILON
                    && b.left <= a.right + EPSILON;

                if same_vertical_track || same_horizontal_track {
                    self.rects[i] = CellRect::new(
                        a.left.min(b.left),
                        a.top.min(b.top),
                        a.right.max(b.right),
                        a.bottom.max(b.bottom),
                        a.alpha.max(b.alpha),
                    );

                    for k in j..(self.rect_count - 1) {
                        self.rects[k] = self.rects[k + 1];
                    }
                    self.rects[self.rect_count - 1] = EMPTY_CELL_RECT;
                    self.rect_count -= 1;
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
    }
}

/// Stroke weight for one arm of a box-drawing connector: light is 1x the
/// base stroke width, heavy is 2x, and double is two parallel light lines
/// separated by one light-width gap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoxLineStyle {
    None,
    Light,
    Heavy,
    Double,
}

impl BoxLineStyle {
    fn is_double(self) -> bool {
        self == Self::Double
    }

    fn is_heavy(self) -> bool {
        self == Self::Heavy
    }
}

/// Four-arm style descriptor for a rectangular box-drawing character.
/// Rounded corners (U+256D-U+2570) and diagonals (U+2571-U+2573) are not
/// representable here and return `None` from `box_draw_segments`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BoxDrawSegments {
    up: BoxLineStyle,
    down: BoxLineStyle,
    left: BoxLineStyle,
    right: BoxLineStyle,
}

impl BoxDrawSegments {
    const fn new(
        up: BoxLineStyle,
        down: BoxLineStyle,
        left: BoxLineStyle,
        right: BoxLineStyle,
    ) -> Self {
        Self {
            up,
            down,
            left,
            right,
        }
    }
}

const fn box_segments(
    up: BoxLineStyle,
    down: BoxLineStyle,
    left: BoxLineStyle,
    right: BoxLineStyle,
) -> BoxDrawSegments {
    BoxDrawSegments::new(up, down, left, right)
}

/// Looks up the four-arm style descriptor for a box-drawing codepoint.
/// Returns `None` for rounded corners, diagonals, and anything outside
/// U+2500..U+257F.
#[allow(clippy::too_many_lines)]
fn box_draw_segments(c: char) -> Option<BoxDrawSegments> {
    use BoxLineStyle::{Double, Heavy, Light, None as Empty};

    let codepoint = c as u32;
    if !(BOX_DRAWING_START..=BOX_DRAWING_END).contains(&codepoint) {
        return None;
    }

    Some(match c {
        '\u{2500}' | '\u{2504}' | '\u{2508}' | '\u{254C}' => {
            box_segments(Empty, Empty, Light, Light)
        }
        '\u{2501}' | '\u{2505}' | '\u{2509}' | '\u{254D}' => {
            box_segments(Empty, Empty, Heavy, Heavy)
        }
        '\u{2502}' | '\u{2506}' | '\u{250A}' | '\u{254E}' => {
            box_segments(Light, Light, Empty, Empty)
        }
        '\u{2503}' | '\u{2507}' | '\u{250B}' | '\u{254F}' => {
            box_segments(Heavy, Heavy, Empty, Empty)
        }
        '\u{250C}' => box_segments(Empty, Light, Empty, Light),
        '\u{250D}' => box_segments(Empty, Light, Empty, Heavy),
        '\u{250E}' => box_segments(Empty, Heavy, Empty, Light),
        '\u{250F}' => box_segments(Empty, Heavy, Empty, Heavy),
        '\u{2510}' => box_segments(Empty, Light, Light, Empty),
        '\u{2511}' => box_segments(Empty, Light, Heavy, Empty),
        '\u{2512}' => box_segments(Empty, Heavy, Light, Empty),
        '\u{2513}' => box_segments(Empty, Heavy, Heavy, Empty),
        '\u{2514}' => box_segments(Light, Empty, Empty, Light),
        '\u{2515}' => box_segments(Light, Empty, Empty, Heavy),
        '\u{2516}' => box_segments(Heavy, Empty, Empty, Light),
        '\u{2517}' => box_segments(Heavy, Empty, Empty, Heavy),
        '\u{2518}' => box_segments(Light, Empty, Light, Empty),
        '\u{2519}' => box_segments(Light, Empty, Heavy, Empty),
        '\u{251A}' => box_segments(Heavy, Empty, Light, Empty),
        '\u{251B}' => box_segments(Heavy, Empty, Heavy, Empty),
        '\u{251C}' => box_segments(Light, Light, Empty, Light),
        '\u{251D}' => box_segments(Light, Light, Empty, Heavy),
        '\u{251E}' => box_segments(Heavy, Light, Empty, Light),
        '\u{251F}' => box_segments(Light, Heavy, Empty, Light),
        '\u{2520}' => box_segments(Heavy, Heavy, Empty, Light),
        '\u{2521}' => box_segments(Light, Heavy, Empty, Heavy),
        '\u{2522}' => box_segments(Heavy, Light, Empty, Heavy),
        '\u{2523}' => box_segments(Heavy, Heavy, Empty, Heavy),
        '\u{2524}' => box_segments(Light, Light, Light, Empty),
        '\u{2525}' => box_segments(Light, Light, Heavy, Empty),
        '\u{2526}' => box_segments(Heavy, Light, Light, Empty),
        '\u{2527}' => box_segments(Light, Heavy, Light, Empty),
        '\u{2528}' => box_segments(Heavy, Heavy, Light, Empty),
        '\u{2529}' => box_segments(Light, Heavy, Heavy, Empty),
        '\u{252A}' => box_segments(Heavy, Light, Heavy, Empty),
        '\u{252B}' => box_segments(Heavy, Heavy, Heavy, Empty),
        '\u{252C}' => box_segments(Empty, Light, Light, Light),
        '\u{252D}' => box_segments(Empty, Light, Heavy, Light),
        '\u{252E}' => box_segments(Empty, Light, Light, Heavy),
        '\u{252F}' => box_segments(Empty, Light, Heavy, Heavy),
        '\u{2530}' => box_segments(Empty, Heavy, Light, Light),
        '\u{2531}' => box_segments(Empty, Heavy, Heavy, Light),
        '\u{2532}' => box_segments(Empty, Heavy, Light, Heavy),
        '\u{2533}' => box_segments(Empty, Heavy, Heavy, Heavy),
        '\u{2534}' => box_segments(Light, Empty, Light, Light),
        '\u{2535}' => box_segments(Light, Empty, Heavy, Light),
        '\u{2536}' => box_segments(Light, Empty, Light, Heavy),
        '\u{2537}' => box_segments(Light, Empty, Heavy, Heavy),
        '\u{2538}' => box_segments(Heavy, Empty, Light, Light),
        '\u{2539}' => box_segments(Heavy, Empty, Heavy, Light),
        '\u{253A}' => box_segments(Heavy, Empty, Light, Heavy),
        '\u{253B}' => box_segments(Heavy, Empty, Heavy, Heavy),
        '\u{253C}' => box_segments(Light, Light, Light, Light),
        '\u{253D}' => box_segments(Light, Light, Heavy, Light),
        '\u{253E}' => box_segments(Light, Light, Light, Heavy),
        '\u{253F}' => box_segments(Light, Light, Heavy, Heavy),
        '\u{2540}' => box_segments(Heavy, Light, Light, Light),
        '\u{2541}' => box_segments(Light, Heavy, Light, Light),
        '\u{2542}' => box_segments(Heavy, Heavy, Light, Light),
        '\u{2543}' => box_segments(Heavy, Light, Heavy, Light),
        '\u{2544}' => box_segments(Heavy, Light, Light, Heavy),
        '\u{2545}' => box_segments(Light, Heavy, Heavy, Light),
        '\u{2546}' => box_segments(Light, Heavy, Light, Heavy),
        '\u{2547}' => box_segments(Light, Heavy, Heavy, Heavy),
        '\u{2548}' => box_segments(Heavy, Light, Heavy, Heavy),
        '\u{2549}' => box_segments(Heavy, Heavy, Heavy, Light),
        '\u{254A}' => box_segments(Heavy, Heavy, Light, Heavy),
        '\u{254B}' => box_segments(Heavy, Heavy, Heavy, Heavy),
        '\u{2550}' => box_segments(Empty, Empty, Double, Double),
        '\u{2551}' => box_segments(Double, Double, Empty, Empty),
        '\u{2552}' => box_segments(Empty, Light, Empty, Double),
        '\u{2553}' => box_segments(Empty, Double, Empty, Light),
        '\u{2554}' => box_segments(Empty, Double, Empty, Double),
        '\u{2555}' => box_segments(Empty, Light, Double, Empty),
        '\u{2556}' => box_segments(Empty, Double, Light, Empty),
        '\u{2557}' => box_segments(Empty, Double, Double, Empty),
        '\u{2558}' => box_segments(Light, Empty, Empty, Double),
        '\u{2559}' => box_segments(Double, Empty, Empty, Light),
        '\u{255A}' => box_segments(Double, Empty, Empty, Double),
        '\u{255B}' => box_segments(Light, Empty, Double, Empty),
        '\u{255C}' => box_segments(Double, Empty, Light, Empty),
        '\u{255D}' => box_segments(Double, Empty, Double, Empty),
        '\u{255E}' => box_segments(Light, Light, Empty, Double),
        '\u{255F}' => box_segments(Double, Double, Empty, Light),
        '\u{2560}' => box_segments(Double, Double, Empty, Double),
        '\u{2561}' => box_segments(Light, Light, Double, Empty),
        '\u{2562}' => box_segments(Double, Double, Light, Empty),
        '\u{2563}' => box_segments(Double, Double, Double, Empty),
        '\u{2564}' => box_segments(Empty, Light, Double, Double),
        '\u{2565}' => box_segments(Empty, Double, Light, Light),
        '\u{2566}' => box_segments(Empty, Double, Double, Double),
        '\u{2567}' => box_segments(Light, Empty, Double, Double),
        '\u{2568}' => box_segments(Double, Empty, Light, Light),
        '\u{2569}' => box_segments(Double, Empty, Double, Double),
        '\u{256A}' => box_segments(Light, Light, Double, Double),
        '\u{256B}' => box_segments(Double, Double, Light, Light),
        '\u{256C}' => box_segments(Double, Double, Double, Double),
        '\u{256D}'..='\u{2570}' => return None,
        '\u{2571}'..='\u{2573}' => return None,
        '\u{2574}' => box_segments(Empty, Empty, Light, Empty),
        '\u{2575}' => box_segments(Light, Empty, Empty, Empty),
        '\u{2576}' => box_segments(Empty, Empty, Empty, Light),
        '\u{2577}' => box_segments(Empty, Light, Empty, Empty),
        '\u{2578}' => box_segments(Empty, Empty, Heavy, Empty),
        '\u{2579}' => box_segments(Heavy, Empty, Empty, Empty),
        '\u{257A}' => box_segments(Empty, Empty, Empty, Heavy),
        '\u{257B}' => box_segments(Empty, Heavy, Empty, Empty),
        '\u{257C}' => box_segments(Empty, Empty, Light, Heavy),
        '\u{257D}' => box_segments(Light, Heavy, Empty, Empty),
        '\u{257E}' => box_segments(Empty, Empty, Heavy, Light),
        '\u{257F}' => box_segments(Heavy, Light, Empty, Empty),
        _ => return None,
    })
}

/// Pushes a rectangle into `geometry`, converting absolute pixel
/// coordinates to cell-relative fractions (0.0..1.0). Clamps to cell
/// bounds and silently discards zero-area results.
fn push_cell_rect_px(
    geometry: &mut CellGeometry,
    left_px: f32,
    top_px: f32,
    right_px: f32,
    bottom_px: f32,
    cell_width: f32,
    cell_height: f32,
) {
    let left = left_px.clamp(0.0, cell_width);
    let right = right_px.clamp(0.0, cell_width);
    let top = top_px.clamp(0.0, cell_height);
    let bottom = bottom_px.clamp(0.0, cell_height);

    if right <= left || bottom <= top {
        return;
    }

    geometry.push_rect(CellRect::new(
        left / cell_width,
        top / cell_height,
        right / cell_width,
        bottom / cell_height,
        1.0,
    ));
}

/// Converts a `BoxDrawSegments` descriptor into cell-relative rectangles
/// using Ghostty's `linesChar` edge placement. Each arm is built
/// independently, then overlapping collinear runs are merged back together
/// so simple glyphs stay compact while mixed light/heavy/double connectors
/// keep Ghostty's join logic.
fn box_draw_geometry(
    segments: BoxDrawSegments,
    cell_width: f32,
    cell_height: f32,
    font_size: f32,
) -> CellGeometry {
    use BoxLineStyle::{Double, Heavy, Light, None as Empty};

    let light_px = (font_size * 0.0675).ceil().max(1.0);
    let heavy_px = light_px * 2.0;

    let h_light_top = ((cell_height - light_px).max(0.0)) / 2.0;
    let h_light_bottom = (h_light_top + light_px).min(cell_height);
    let h_heavy_top = ((cell_height - heavy_px).max(0.0)) / 2.0;
    let h_heavy_bottom = (h_heavy_top + heavy_px).min(cell_height);
    let h_double_top = (h_light_top - light_px).max(0.0);
    let h_double_bottom = (h_light_bottom + light_px).min(cell_height);

    let v_light_left = ((cell_width - light_px).max(0.0)) / 2.0;
    let v_light_right = (v_light_left + light_px).min(cell_width);
    let v_heavy_left = ((cell_width - heavy_px).max(0.0)) / 2.0;
    let v_heavy_right = (v_heavy_left + heavy_px).min(cell_width);
    let v_double_left = (v_light_left - light_px).max(0.0);
    let v_double_right = (v_light_right + light_px).min(cell_width);

    let up_bottom = if segments.left.is_heavy() || segments.right.is_heavy() {
        h_heavy_bottom
    } else if segments.left != segments.right || segments.down == segments.up {
        if segments.left.is_double() || segments.right.is_double() {
            h_double_bottom
        } else {
            h_light_bottom
        }
    } else if segments.left == Empty && segments.right == Empty {
        h_light_bottom
    } else {
        h_light_top
    };

    let down_top = if segments.left.is_heavy() || segments.right.is_heavy() {
        h_heavy_top
    } else if segments.left != segments.right || segments.up == segments.down {
        if segments.left.is_double() || segments.right.is_double() {
            h_double_top
        } else {
            h_light_top
        }
    } else if segments.left == Empty && segments.right == Empty {
        h_light_top
    } else {
        h_light_bottom
    };

    let left_right = if segments.up.is_heavy() || segments.down.is_heavy() {
        v_heavy_right
    } else if segments.up != segments.down || segments.left == segments.right {
        if segments.up.is_double() || segments.down.is_double() {
            v_double_right
        } else {
            v_light_right
        }
    } else if segments.up == Empty && segments.down == Empty {
        v_light_right
    } else {
        v_light_left
    };

    let right_left = if segments.up.is_heavy() || segments.down.is_heavy() {
        v_heavy_left
    } else if segments.up != segments.down || segments.right == segments.left {
        if segments.up.is_double() || segments.down.is_double() {
            v_double_left
        } else {
            v_light_left
        }
    } else if segments.up == Empty && segments.down == Empty {
        v_light_left
    } else {
        v_light_right
    };

    let mut geometry = CellGeometry::empty();

    match segments.up {
        Empty => {}
        Light => push_cell_rect_px(
            &mut geometry,
            v_light_left,
            0.0,
            v_light_right,
            up_bottom,
            cell_width,
            cell_height,
        ),
        Heavy => push_cell_rect_px(
            &mut geometry,
            v_heavy_left,
            0.0,
            v_heavy_right,
            up_bottom,
            cell_width,
            cell_height,
        ),
        Double => {
            let left_bottom = if segments.left == Double {
                h_light_top
            } else {
                up_bottom
            };
            let right_bottom = if segments.right == Double {
                h_light_top
            } else {
                up_bottom
            };
            push_cell_rect_px(
                &mut geometry,
                v_double_left,
                0.0,
                v_light_left,
                left_bottom,
                cell_width,
                cell_height,
            );
            push_cell_rect_px(
                &mut geometry,
                v_light_right,
                0.0,
                v_double_right,
                right_bottom,
                cell_width,
                cell_height,
            );
        }
    }

    match segments.right {
        Empty => {}
        Light => push_cell_rect_px(
            &mut geometry,
            right_left,
            h_light_top,
            cell_width,
            h_light_bottom,
            cell_width,
            cell_height,
        ),
        Heavy => push_cell_rect_px(
            &mut geometry,
            right_left,
            h_heavy_top,
            cell_width,
            h_heavy_bottom,
            cell_width,
            cell_height,
        ),
        Double => {
            let top_left = if segments.up == Double {
                v_light_right
            } else {
                right_left
            };
            let bottom_left = if segments.down == Double {
                v_light_right
            } else {
                right_left
            };
            push_cell_rect_px(
                &mut geometry,
                top_left,
                h_double_top,
                cell_width,
                h_light_top,
                cell_width,
                cell_height,
            );
            push_cell_rect_px(
                &mut geometry,
                bottom_left,
                h_light_bottom,
                cell_width,
                h_double_bottom,
                cell_width,
                cell_height,
            );
        }
    }

    match segments.down {
        Empty => {}
        Light => push_cell_rect_px(
            &mut geometry,
            v_light_left,
            down_top,
            v_light_right,
            cell_height,
            cell_width,
            cell_height,
        ),
        Heavy => push_cell_rect_px(
            &mut geometry,
            v_heavy_left,
            down_top,
            v_heavy_right,
            cell_height,
            cell_width,
            cell_height,
        ),
        Double => {
            let left_top = if segments.left == Double {
                h_light_bottom
            } else {
                down_top
            };
            let right_top = if segments.right == Double {
                h_light_bottom
            } else {
                down_top
            };
            push_cell_rect_px(
                &mut geometry,
                v_double_left,
                left_top,
                v_light_left,
                cell_height,
                cell_width,
                cell_height,
            );
            push_cell_rect_px(
                &mut geometry,
                v_light_right,
                right_top,
                v_double_right,
                cell_height,
                cell_width,
                cell_height,
            );
        }
    }

    match segments.left {
        Empty => {}
        Light => push_cell_rect_px(
            &mut geometry,
            0.0,
            h_light_top,
            left_right,
            h_light_bottom,
            cell_width,
            cell_height,
        ),
        Heavy => push_cell_rect_px(
            &mut geometry,
            0.0,
            h_heavy_top,
            left_right,
            h_heavy_bottom,
            cell_width,
            cell_height,
        ),
        Double => {
            let top_right = if segments.up == Double {
                v_light_left
            } else {
                left_right
            };
            let bottom_right = if segments.down == Double {
                v_light_left
            } else {
                left_right
            };
            push_cell_rect_px(
                &mut geometry,
                0.0,
                h_double_top,
                top_right,
                h_light_top,
                cell_width,
                cell_height,
            );
            push_cell_rect_px(
                &mut geometry,
                0.0,
                h_light_bottom,
                bottom_right,
                h_double_bottom,
                cell_width,
                cell_height,
            );
        }
    }

    geometry.merge_collinear_overlaps();

    geometry
}

/// Looks up `box_draw_segments` and, if the codepoint is a rectangular
/// connector, converts the descriptor into cell-relative geometry. Returns
/// `None` for rounded corners, diagonals, and non-box-drawing characters.
fn box_draw_geometry_for_char(
    c: char,
    cell_width: f32,
    cell_height: f32,
    font_size: f32,
) -> Option<CellGeometry> {
    box_draw_segments(c)
        .map(|segments| box_draw_geometry(segments, cell_width, cell_height, font_size))
}

fn full_cell_rect(alpha: f32) -> CellRect {
    CellRect::new(0.0, 0.0, 1.0, 1.0, alpha)
}

fn vertical_fill_from_bottom(fraction: f32) -> CellGeometry {
    CellGeometry::one(CellRect::new(0.0, 1.0 - fraction, 1.0, 1.0, 1.0))
}

fn horizontal_fill_from_left(fraction: f32) -> CellGeometry {
    CellGeometry::one(CellRect::new(0.0, 0.0, fraction, 1.0, 1.0))
}

fn quadrants(mask: u8) -> CellGeometry {
    let mut rects = [EMPTY_CELL_RECT; 8];
    let mut count = 0;

    if mask & QUAD_UPPER_LEFT != 0 {
        rects[count] = CellRect::new(0.0, 0.0, 0.5, 0.5, 1.0);
        count += 1;
    }
    if mask & QUAD_UPPER_RIGHT != 0 {
        rects[count] = CellRect::new(0.5, 0.0, 1.0, 0.5, 1.0);
        count += 1;
    }
    if mask & QUAD_LOWER_LEFT != 0 {
        rects[count] = CellRect::new(0.0, 0.5, 0.5, 1.0, 1.0);
        count += 1;
    }
    if mask & QUAD_LOWER_RIGHT != 0 {
        rects[count] = CellRect::new(0.5, 0.5, 1.0, 1.0, 1.0);
        count += 1;
    }

    CellGeometry {
        rects,
        rect_count: count,
    }
}

/// Cell-relative geometry for a block-element character (U+2580-259F):
/// halves, eighths, quadrants, and shaded blocks. Returns `None` outside
/// that range.
fn block_element_geometry(c: char) -> Option<CellGeometry> {
    let codepoint = c as u32;
    if !(BLOCK_ELEMENTS_START..=BLOCK_ELEMENTS_END).contains(&codepoint) {
        return None;
    }

    Some(match c {
        '\u{2580}' => CellGeometry::one(CellRect::new(0.0, 0.0, 1.0, 0.5, 1.0)),
        '\u{2581}' => vertical_fill_from_bottom(1.0 / 8.0),
        '\u{2582}' => vertical_fill_from_bottom(2.0 / 8.0),
        '\u{2583}' => vertical_fill_from_bottom(3.0 / 8.0),
        '\u{2584}' => vertical_fill_from_bottom(4.0 / 8.0),
        '\u{2585}' => vertical_fill_from_bottom(5.0 / 8.0),
        '\u{2586}' => vertical_fill_from_bottom(6.0 / 8.0),
        '\u{2587}' => vertical_fill_from_bottom(7.0 / 8.0),
        '\u{2588}' => CellGeometry::one(full_cell_rect(1.0)),
        '\u{2589}' => horizontal_fill_from_left(7.0 / 8.0),
        '\u{258A}' => horizontal_fill_from_left(6.0 / 8.0),
        '\u{258B}' => horizontal_fill_from_left(5.0 / 8.0),
        '\u{258C}' => horizontal_fill_from_left(4.0 / 8.0),
        '\u{258D}' => horizontal_fill_from_left(3.0 / 8.0),
        '\u{258E}' => horizontal_fill_from_left(2.0 / 8.0),
        '\u{258F}' => horizontal_fill_from_left(1.0 / 8.0),
        '\u{2590}' => CellGeometry::one(CellRect::new(0.5, 0.0, 1.0, 1.0, 1.0)),
        '\u{2591}' => CellGeometry::one(full_cell_rect(0.25)),
        '\u{2592}' => CellGeometry::one(full_cell_rect(0.50)),
        '\u{2593}' => CellGeometry::one(full_cell_rect(0.75)),
        '\u{2594}' => CellGeometry::one(CellRect::new(0.0, 0.0, 1.0, 1.0 / 8.0, 1.0)),
        '\u{2595}' => CellGeometry::one(CellRect::new(7.0 / 8.0, 0.0, 1.0, 1.0, 1.0)),
        '\u{2596}' => quadrants(QUAD_LOWER_LEFT),
        '\u{2597}' => quadrants(QUAD_LOWER_RIGHT),
        '\u{2598}' => quadrants(QUAD_UPPER_LEFT),
        '\u{2599}' => quadrants(QUAD_UPPER_LEFT | QUAD_LOWER_LEFT | QUAD_LOWER_RIGHT),
        '\u{259A}' => quadrants(QUAD_UPPER_LEFT | QUAD_LOWER_RIGHT),
        '\u{259B}' => quadrants(QUAD_UPPER_LEFT | QUAD_UPPER_RIGHT | QUAD_LOWER_LEFT),
        '\u{259C}' => quadrants(QUAD_UPPER_LEFT | QUAD_UPPER_RIGHT | QUAD_LOWER_RIGHT),
        '\u{259D}' => quadrants(QUAD_UPPER_RIGHT),
        '\u{259E}' => quadrants(QUAD_UPPER_RIGHT | QUAD_LOWER_LEFT),
        '\u{259F}' => quadrants(QUAD_UPPER_RIGHT | QUAD_LOWER_LEFT | QUAD_LOWER_RIGHT),
        _ => return None,
    })
}

fn reverse_lower_six_bits(value: u8) -> u8 {
    ((value & 0b00_0001) << 5)
        | ((value & 0b00_0010) << 3)
        | ((value & 0b00_0100) << 1)
        | ((value & 0b00_1000) >> 1)
        | ((value & 0b01_0000) >> 3)
        | ((value & 0b10_0000) >> 5)
}

/// Decodes a Legacy-Computing sextant mosaic codepoint into its packed
/// 2x3 dot pattern: bit N set means the sextant cell at that position is
/// *empty* (the XOR-invert below flips the raw offset-derived bit pattern,
/// which encodes filled positions, into an empty-position mask that
/// `sextant_geometry` can skip over directly).
fn sextant_char_to_packed(ch: char) -> Option<u8> {
    let codepoint = ch as u32;
    if !(SEXTANT_MOSAIC_START..=SEXTANT_MOSAIC_END).contains(&codepoint) {
        return None;
    }

    let offset = codepoint - SEXTANT_MOSAIC_START;
    let sextant = (offset + 1 + u32::from(offset >= 20) + u32::from(offset >= 40)) as u8;
    Some(reverse_lower_six_bits(sextant) ^ 0b11_1111)
}

/// Cell-relative geometry for a sextant mosaic character: up to 6 rects on
/// a 2-column x 3-row sub-grid.
fn sextant_geometry(ch: char) -> Option<CellGeometry> {
    let packed = sextant_char_to_packed(ch)?;
    let mut geometry = CellGeometry::empty();

    for row in 0..3usize {
        for col in 0..2usize {
            let bit = 5usize - (row * 2 + col);
            if (packed & (1 << bit)) != 0 {
                continue;
            }
            geometry.push_rect(CellRect::new(
                col as f32 / 2.0,
                row as f32 / 3.0,
                (col + 1) as f32 / 2.0,
                (row + 1) as f32 / 3.0,
                1.0,
            ));
        }
    }

    Some(geometry)
}

fn is_braille_pattern_char(c: char) -> bool {
    (BRAILLE_PATTERNS_START..=BRAILLE_PATTERNS_END).contains(&(c as u32))
}

/// Cell-relative geometry for a Braille pattern character: up to 8 dots on
/// a 2x4 sub-grid. Returns `None` for the blank pattern (U+2800) and
/// anything outside the Braille block.
fn braille_geometry(c: char) -> Option<CellGeometry> {
    if !is_braille_pattern_char(c) {
        return None;
    }

    let pattern = (c as u32 - BRAILLE_PATTERNS_START) as u8;
    if pattern == 0 {
        return None;
    }

    const DOT_WIDTH: f32 = 0.24;
    const DOT_HEIGHT: f32 = 0.16;
    const LEFT_X: f32 = 0.22;
    const RIGHT_X: f32 = 0.64;
    const ROW_Y: [f32; 4] = [0.08, 0.31, 0.54, 0.77];
    const DOT_MASKS: [(u8, f32, f32); 8] = [
        (0b0000_0001, LEFT_X, ROW_Y[0]),
        (0b0000_0010, LEFT_X, ROW_Y[1]),
        (0b0000_0100, LEFT_X, ROW_Y[2]),
        (0b0100_0000, LEFT_X, ROW_Y[3]),
        (0b0000_1000, RIGHT_X, ROW_Y[0]),
        (0b0001_0000, RIGHT_X, ROW_Y[1]),
        (0b0010_0000, RIGHT_X, ROW_Y[2]),
        (0b1000_0000, RIGHT_X, ROW_Y[3]),
    ];

    let mut geometry = CellGeometry::empty();
    for (mask, left, top) in DOT_MASKS {
        if pattern & mask == 0 {
            continue;
        }
        geometry.push_rect(CellRect::new(
            left,
            top,
            (left + DOT_WIDTH).min(1.0),
            (top + DOT_HEIGHT).min(1.0),
            1.0,
        ));
    }
    Some(geometry)
}

fn rounded_corner_char(c: char) -> bool {
    matches!(c, '\u{256D}' | '\u{256E}' | '\u{256F}' | '\u{2570}')
}

fn diagonal_char(c: char) -> bool {
    matches!(c, '\u{2571}' | '\u{2572}' | '\u{2573}')
}

/// Resolved path geometry for a rounded-corner box-drawing glyph. The path
/// is: `start` -> straight to `curve_start` -> cubic Bezier (`control_a`,
/// `control_b`) -> `curve_end` -> straight to `end`, giving a short stub on
/// each cell edge that aligns with adjacent straight box lines, joined by a
/// quarter-circle arc in the cell interior.
#[derive(Clone, Copy, Debug)]
struct RoundedCornerPathSpec {
    start: Point<Pixels>,
    curve_start: Point<Pixels>,
    control_a: Point<Pixels>,
    control_b: Point<Pixels>,
    curve_end: Point<Pixels>,
    end: Point<Pixels>,
    stroke_width: Pixels,
}

/// Resolved path geometry for a diagonal box-drawing glyph: a single line
/// segment from `start` to `end`, overshooting the cell boundary by a
/// slope-dependent amount so adjacent diagonal cells join without pixel
/// gaps.
#[derive(Clone, Copy, Debug)]
struct DiagonalPathSpec {
    start: Point<Pixels>,
    end: Point<Pixels>,
    stroke_width: Pixels,
}

/// Snaps a cell's bounds to whole logical pixels, discarding
/// zero-area results.
fn snapped_cell_bounds(cell_bounds: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
    let origin_x: f32 = cell_bounds.origin.x.into();
    let origin_y: f32 = cell_bounds.origin.y.into();
    let width: f32 = cell_bounds.size.width.into();
    let height: f32 = cell_bounds.size.height.into();

    let left = origin_x.round();
    let right = (origin_x + width).round();
    let top = origin_y.round();
    let bottom = (origin_y + height).round();

    let snapped_width = right - left;
    let snapped_height = bottom - top;
    if snapped_width <= 0.0 || snapped_height <= 0.0 {
        return None;
    }

    Some(Bounds {
        origin: point(px(left), px(top)),
        size: Size {
            width: px(snapped_width),
            height: px(snapped_height),
        },
    })
}

/// Resolves a cell-relative [`CellRect`] against a cell's absolute bounds,
/// snapping every edge to a whole logical pixel so adjacent fills butt up
/// against each other with no anti-aliased seam. Discards zero-area
/// results.
fn snapped_rect_bounds(cell_bounds: Bounds<Pixels>, rect: CellRect) -> Option<Bounds<Pixels>> {
    let origin_x: f32 = cell_bounds.origin.x.into();
    let origin_y: f32 = cell_bounds.origin.y.into();
    let cell_width: f32 = cell_bounds.size.width.into();
    let cell_height: f32 = cell_bounds.size.height.into();

    let left = (origin_x + cell_width * rect.left).round();
    let right = (origin_x + cell_width * rect.right).round();
    let top = (origin_y + cell_height * rect.top).round();
    let bottom = (origin_y + cell_height * rect.bottom).round();

    let width = right - left;
    let height = bottom - top;
    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    Some(Bounds {
        origin: point(px(left), px(top)),
        size: Size {
            width: px(width),
            height: px(height),
        },
    })
}

/// Snaps the center of a stroke to the nearest *device* pixel rather than
/// the nearest logical pixel: multiplies into device-pixel space, rounds,
/// then divides back. At an integer `scale_factor` this is equivalent to
/// rounding in logical space, but at a fractional factor (e.g. 1.5x) plain
/// logical-pixel rounding can still leave a 1-device-pixel-wide stroke
/// straddling two device pixels, blurring it under anti-aliasing. See this
/// module's doc comment: independently derived from the survey's
/// description of the technique, not ported from any GPL source.
fn snap_center_to_device_pixel(
    origin: Pixels,
    size: Pixels,
    stroke_width: Pixels,
    scale_factor: f32,
) -> Pixels {
    let origin_px: f32 = origin.into();
    let size_px: f32 = size.into();
    let stroke_px: f32 = stroke_width.into();
    let center_px = origin_px + size_px / 2.0;

    let snap_edge = |value: f32| (value * scale_factor).round() / scale_factor;
    let min_px = snap_edge(center_px - stroke_px / 2.0);
    let max_px = snap_edge(center_px + stroke_px / 2.0);
    px((min_px + max_px) / 2.0)
}

/// Stroke width for box-drawing lines, rounded corners, and diagonals: a
/// fixed fraction of the font size, matching `box_draw_geometry`'s own
/// light-stroke width so a rounded corner or diagonal joins a straight box
/// line of the same weight without a visible step.
fn stroke_width_for_font_size(font_size: Pixels) -> Pixels {
    px((Into::<f32>::into(font_size) * 0.0675).ceil().max(1.0))
}

fn rounded_corner_path_spec(
    cell_bounds: Bounds<Pixels>,
    glyph: char,
    stroke_width: Pixels,
    scale_factor: f32,
) -> Option<RoundedCornerPathSpec> {
    let cell_bounds = snapped_cell_bounds(cell_bounds)?;
    let origin = cell_bounds.origin;
    let width = cell_bounds.size.width;
    let height = cell_bounds.size.height;
    let width_px: f32 = width.into();
    let height_px: f32 = height.into();
    let stroke_px: f32 = stroke_width.into();
    let radius = px(((width_px.min(height_px) - stroke_px).max(0.0)) / 2.0);
    let ctrl_offset = radius / 4.0;
    let center_x = snap_center_to_device_pixel(origin.x, width, stroke_width, scale_factor);
    let center_y = snap_center_to_device_pixel(origin.y, height, stroke_width, scale_factor);
    let left_center = point(origin.x, center_y);
    let right_center = point(origin.x + width, center_y);
    let top_center = point(center_x, origin.y);
    let bottom_center = point(center_x, origin.y + height);

    match glyph {
        '\u{256D}' => Some(RoundedCornerPathSpec {
            start: bottom_center,
            curve_start: point(center_x, center_y + radius),
            control_a: point(center_x, center_y + ctrl_offset),
            control_b: point(center_x + ctrl_offset, center_y),
            curve_end: point(center_x + radius, center_y),
            end: right_center,
            stroke_width,
        }),
        '\u{256E}' => Some(RoundedCornerPathSpec {
            start: bottom_center,
            curve_start: point(center_x, center_y + radius),
            control_a: point(center_x, center_y + ctrl_offset),
            control_b: point(center_x - ctrl_offset, center_y),
            curve_end: point(center_x - radius, center_y),
            end: left_center,
            stroke_width,
        }),
        '\u{256F}' => Some(RoundedCornerPathSpec {
            start: top_center,
            curve_start: point(center_x, center_y - radius),
            control_a: point(center_x, center_y - ctrl_offset),
            control_b: point(center_x - ctrl_offset, center_y),
            curve_end: point(center_x - radius, center_y),
            end: left_center,
            stroke_width,
        }),
        '\u{2570}' => Some(RoundedCornerPathSpec {
            start: top_center,
            curve_start: point(center_x, center_y - radius),
            control_a: point(center_x, center_y - ctrl_offset),
            control_b: point(center_x + ctrl_offset, center_y),
            curve_end: point(center_x + radius, center_y),
            end: right_center,
            stroke_width,
        }),
        _ => None,
    }
}

fn diagonal_path_specs(
    cell_bounds: Bounds<Pixels>,
    glyph: char,
    stroke_width: Pixels,
) -> Option<(DiagonalPathSpec, Option<DiagonalPathSpec>)> {
    let cell_bounds = snapped_cell_bounds(cell_bounds)?;
    let origin = cell_bounds.origin;
    let width = cell_bounds.size.width;
    let height = cell_bounds.size.height;
    let width_px: f32 = width.into();
    let height_px: f32 = height.into();
    if width_px <= 0.0 || height_px <= 0.0 {
        return None;
    }

    let slope_x = px(0.5 * (width_px / height_px).min(1.0));
    let slope_y = px(0.5 * (height_px / width_px).min(1.0));

    let upper_right_to_lower_left = DiagonalPathSpec {
        start: point(origin.x + width + slope_x, origin.y - slope_y),
        end: point(origin.x - slope_x, origin.y + height + slope_y),
        stroke_width,
    };
    let upper_left_to_lower_right = DiagonalPathSpec {
        start: point(origin.x - slope_x, origin.y - slope_y),
        end: point(origin.x + width + slope_x, origin.y + height + slope_y),
        stroke_width,
    };

    match glyph {
        '\u{2571}' => Some((upper_right_to_lower_left, None)),
        '\u{2572}' => Some((upper_left_to_lower_right, None)),
        '\u{2573}' => Some((upper_right_to_lower_left, Some(upper_left_to_lower_right))),
        _ => None,
    }
}

// ---------------------------------------------------------------------
// Paint glue: resolves the pure geometry above against a `Window` and
// paints it. Not unit-tested (needs a live `Window`) -- see the
// `HORIZON_GPUI_DUMP`/`HORIZON_GPUI_DRIVE` caveat in `paint_terminal`'s own
// doc comment for why pixel output stays a manual-verification concern.
// ---------------------------------------------------------------------

fn paint_cell_geometry(
    window: &mut Window,
    cell_bounds: Bounds<Pixels>,
    geometry: &CellGeometry,
    color: Hsla,
) {
    for rect in geometry.rects() {
        if let Some(bounds) = snapped_rect_bounds(cell_bounds, *rect) {
            let mut fill_color = color;
            fill_color.a *= rect.alpha;
            window.paint_quad(gpui::fill(bounds, fill_color));
        }
    }
}

fn paint_rounded_corner(
    window: &mut Window,
    cell_bounds: Bounds<Pixels>,
    glyph: char,
    color: Hsla,
    font_size: Pixels,
    scale_factor: f32,
) {
    let stroke_width = stroke_width_for_font_size(font_size);
    let Some(spec) = rounded_corner_path_spec(cell_bounds, glyph, stroke_width, scale_factor)
    else {
        return;
    };

    let mut builder = PathBuilder::stroke(spec.stroke_width);
    builder.move_to(spec.start);
    builder.line_to(spec.curve_start);
    builder.cubic_bezier_to(spec.curve_end, spec.control_a, spec.control_b);
    builder.line_to(spec.end);

    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn paint_diagonal(
    window: &mut Window,
    cell_bounds: Bounds<Pixels>,
    glyph: char,
    color: Hsla,
    font_size: Pixels,
) {
    let stroke_width = stroke_width_for_font_size(font_size);
    let Some((primary, secondary)) = diagonal_path_specs(cell_bounds, glyph, stroke_width) else {
        return;
    };

    for spec in [Some(primary), secondary].into_iter().flatten() {
        let mut builder = PathBuilder::stroke(spec.stroke_width);
        builder.move_to(spec.start);
        builder.line_to(spec.end);

        if let Ok(path) = builder.build() {
            window.paint_path(path, color);
        }
    }
}

/// Whether `ch` is covered by this module's geometric synthesis --
/// `super::paint_terminal`'s span loop uses this to decide whether a
/// character should be painted via [`paint_glyph`] instead of shaped as a
/// font glyph.
pub(crate) fn is_geometric(ch: char) -> bool {
    box_draw_segments(ch).is_some()
        || rounded_corner_char(ch)
        || diagonal_char(ch)
        || block_element_geometry(ch).is_some()
        || sextant_char_to_packed(ch).is_some()
        || braille_geometry(ch).is_some()
}

/// Paints `ch` geometrically into `cell_bounds` (its already-positioned,
/// already-sized cell rectangle) in `color`. Returns `false` (having
/// painted nothing) if `ch` is not covered by this module -- callers
/// should only reach this after checking [`is_geometric`].
pub(crate) fn paint_glyph(
    window: &mut Window,
    cell_bounds: Bounds<Pixels>,
    ch: char,
    color: Hsla,
    font_size: Pixels,
    scale_factor: f32,
) -> bool {
    if rounded_corner_char(ch) {
        paint_rounded_corner(window, cell_bounds, ch, color, font_size, scale_factor);
        return true;
    }
    if diagonal_char(ch) {
        paint_diagonal(window, cell_bounds, ch, color, font_size);
        return true;
    }

    let cell_width: f32 = cell_bounds.size.width.into();
    let cell_height: f32 = cell_bounds.size.height.into();
    let font_size_px: f32 = font_size.into();

    if let Some(geometry) = box_draw_geometry_for_char(ch, cell_width, cell_height, font_size_px) {
        paint_cell_geometry(window, cell_bounds, &geometry, color);
        return true;
    }
    if let Some(geometry) = block_element_geometry(ch) {
        paint_cell_geometry(window, cell_bounds, &geometry, color);
        return true;
    }
    if let Some(geometry) = sextant_geometry(ch) {
        paint_cell_geometry(window, cell_bounds, &geometry, color);
        return true;
    }
    if let Some(geometry) = braille_geometry(ch) {
        paint_cell_geometry(window, cell_bounds, &geometry, color);
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_f32_eq(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn block_element_geometry_is_complete_for_unicode_range() {
        for codepoint in BLOCK_ELEMENTS_START..=BLOCK_ELEMENTS_END {
            let glyph = char::from_u32(codepoint).expect("valid block-element codepoint");
            assert!(
                block_element_geometry(glyph).is_some(),
                "missing geometry for U+{codepoint:04X}"
            );
        }
    }

    #[test]
    fn box_draw_segments_covers_expected_range() {
        for codepoint in BOX_DRAWING_START..=BOX_DRAWING_END {
            let glyph = char::from_u32(codepoint).expect("valid box-drawing codepoint");
            assert!(
                rounded_corner_char(glyph)
                    || diagonal_char(glyph)
                    || box_draw_geometry_for_char(glyph, 10.0, 20.0, 14.0).is_some(),
                "unexpected box-drawing coverage for U+{codepoint:04X}"
            );
        }
    }

    #[test]
    fn fast_path_excludes_non_covered_glyphs() {
        assert!(block_element_geometry('\u{2579}').is_none());
        assert!(block_element_geometry('A').is_none());
        assert!(!is_geometric('A'));
        assert!(!is_geometric(' '));
    }

    #[test]
    fn upper_half_block_geometry_covers_top_half() {
        let geometry = block_element_geometry('\u{2580}').expect("expected block geometry");
        assert_eq!(geometry.rect_count, 1);
        let rect = geometry.rects()[0];
        assert_eq!(rect.left, 0.0);
        assert_eq!(rect.top, 0.0);
        assert_eq!(rect.right, 1.0);
        assert_eq!(rect.bottom, 0.5);
        assert_eq!(rect.alpha, 1.0);
    }

    #[test]
    fn quadrant_block_geometry_covers_one_quarter() {
        let geometry = block_element_geometry('\u{2598}').expect("expected quadrant geometry");
        assert_eq!(geometry.rect_count, 1);
        let rect = geometry.rects()[0];
        assert_eq!(
            (rect.left, rect.top, rect.right, rect.bottom),
            (0.0, 0.0, 0.5, 0.5)
        );
    }

    #[test]
    fn shade_block_geometry_covers_full_cell_with_partial_alpha() {
        let geometry = block_element_geometry('\u{2592}').expect("expected shade geometry");
        assert_eq!(geometry.rect_count, 1);
        let rect = geometry.rects()[0];
        assert_eq!(
            (rect.left, rect.top, rect.right, rect.bottom),
            (0.0, 0.0, 1.0, 1.0)
        );
        assert_eq!(rect.alpha, 0.50);
    }

    #[test]
    fn box_draw_light_horizontal_geometry() {
        let geometry =
            box_draw_geometry_for_char('\u{2500}', 10.0, 20.0, 14.0).expect("expected geometry");

        assert_eq!(geometry.rect_count, 1);
        let rect = geometry.rects()[0];
        assert_f32_eq(rect.left, 0.0);
        assert_f32_eq(rect.right, 1.0);
        assert_f32_eq(rect.top, 0.475);
        assert_f32_eq(rect.bottom, 0.525);
        assert_eq!(rect.alpha, 1.0);
    }

    #[test]
    fn box_draw_heavy_vertical_geometry_uses_double_width_stroke() {
        let light =
            box_draw_geometry_for_char('\u{2502}', 10.0, 20.0, 14.0).expect("expected geometry");
        let heavy =
            box_draw_geometry_for_char('\u{2503}', 10.0, 20.0, 14.0).expect("expected geometry");

        let light_rect = light.rects()[0];
        let heavy_rect = heavy.rects()[0];
        let light_width = light_rect.right - light_rect.left;
        let heavy_width = heavy_rect.right - heavy_rect.left;
        assert_f32_eq(heavy_width, light_width * 2.0);
    }

    #[test]
    fn box_draw_light_cross_geometry_is_a_t_junction_pair() {
        let geometry =
            box_draw_geometry_for_char('\u{253C}', 10.0, 20.0, 14.0).expect("expected geometry");

        assert_eq!(geometry.rect_count, 2);
        let vertical = geometry.rects()[0];
        assert_f32_eq(vertical.left, 0.45);
        assert_f32_eq(vertical.top, 0.0);
        assert_f32_eq(vertical.right, 0.55);
        assert_f32_eq(vertical.bottom, 1.0);

        let horizontal = geometry.rects()[1];
        assert_f32_eq(horizontal.left, 0.0);
        assert_f32_eq(horizontal.top, 0.475);
        assert_f32_eq(horizontal.right, 1.0);
        assert_f32_eq(horizontal.bottom, 0.525);
    }

    #[test]
    fn box_draw_double_cross_geometry() {
        let geometry =
            box_draw_geometry_for_char('\u{256C}', 10.0, 20.0, 14.0).expect("expected geometry");

        assert_eq!(geometry.rect_count, 8);

        let top_left_vertical = geometry.rects()[0];
        assert_f32_eq(top_left_vertical.left, 0.35);
        assert_f32_eq(top_left_vertical.right, 0.45);
        assert_f32_eq(top_left_vertical.top, 0.0);
        assert_f32_eq(top_left_vertical.bottom, 0.475);
    }

    #[test]
    fn box_draw_light_to_heavy_connector_matches_ghostty_join_extents() {
        let geometry =
            box_draw_geometry_for_char('\u{251D}', 10.0, 20.0, 14.0).expect("expected geometry");

        assert_eq!(geometry.rect_count, 2);

        let vertical = geometry.rects()[0];
        assert_f32_eq(vertical.left, 0.45);
        assert_f32_eq(vertical.right, 0.55);
        assert_f32_eq(vertical.top, 0.0);
        assert_f32_eq(vertical.bottom, 1.0);

        let horizontal = geometry.rects()[1];
        assert_f32_eq(horizontal.left, 0.55);
        assert_f32_eq(horizontal.right, 1.0);
        assert_f32_eq(horizontal.top, 0.45);
        assert_f32_eq(horizontal.bottom, 0.55);
    }

    #[test]
    fn box_draw_lines_extend_to_cell_edges() {
        let vertical =
            box_draw_geometry_for_char('\u{2551}', 10.0, 20.0, 14.0).expect("expected geometry");
        assert!(vertical
            .rects()
            .iter()
            .all(|rect| rect.top == 0.0 && rect.bottom == 1.0));

        let horizontal =
            box_draw_geometry_for_char('\u{2550}', 10.0, 20.0, 14.0).expect("expected geometry");
        assert!(horizontal
            .rects()
            .iter()
            .all(|rect| rect.left == 0.0 && rect.right == 1.0));
    }

    #[test]
    fn rounded_top_left_corner_uses_ghostty_style_cubic_path() {
        let bounds = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: Size {
                width: px(10.0),
                height: px(20.0),
            },
        };
        let spec = rounded_corner_path_spec(bounds, '\u{256D}', px(1.0), 1.0)
            .expect("expected path points");

        assert_f32_eq(spec.start.x.into(), 5.5);
        assert_f32_eq(spec.start.y.into(), 20.0);
        assert_f32_eq(spec.curve_start.x.into(), 5.5);
        assert_f32_eq(spec.curve_start.y.into(), 15.0);
        assert_f32_eq(spec.control_a.x.into(), 5.5);
        assert_f32_eq(spec.control_a.y.into(), 11.625);
        assert_f32_eq(spec.control_b.x.into(), 6.625);
        assert_f32_eq(spec.control_b.y.into(), 10.5);
        assert_f32_eq(spec.curve_end.x.into(), 10.0);
        assert_f32_eq(spec.curve_end.y.into(), 10.5);
        assert_f32_eq(spec.end.x.into(), 10.0);
        assert_f32_eq(spec.end.y.into(), 10.5);
    }

    #[test]
    fn rounded_bottom_right_corner_uses_ghostty_style_cubic_path() {
        let bounds = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: Size {
                width: px(20.0),
                height: px(10.0),
            },
        };
        let spec = rounded_corner_path_spec(bounds, '\u{256F}', px(1.0), 1.0)
            .expect("expected path points");

        assert_f32_eq(spec.start.x.into(), 10.5);
        assert_f32_eq(spec.start.y.into(), 0.0);
        assert_f32_eq(spec.curve_end.x.into(), 6.0);
        assert_f32_eq(spec.curve_end.y.into(), 5.5);
        assert_f32_eq(spec.end.x.into(), 0.0);
        assert_f32_eq(spec.end.y.into(), 5.5);
    }

    /// The device-pixel-snapping discipline this module reimplements
    /// independently of termy/paneflow (see the module doc comment): each
    /// stroke *edge* -- not the unsnapped center -- is rounded to the
    /// nearest device pixel (`value * scale_factor` is an integer), then
    /// the two snapped edges are averaged. Golden value derived by hand:
    /// center_px = 0.3 + 11.0/2 = 5.8; edges 5.3/6.3 scale to 7.95/9.45
    /// device pixels, round to 8/9, and scale back to 5.333.../6.0; their
    /// average is 5.666667. At a fractional scale factor this average is
    /// not itself required to land on a device pixel (the two edges'
    /// device-pixel distance can be odd), which is exactly why a
    /// center-only alignment check would be the wrong invariant to test.
    #[test]
    fn stroke_center_snap_rounds_each_edge_to_a_device_pixel_independently() {
        let center = snap_center_to_device_pixel(px(0.3), px(11.0), px(1.0), 1.5);
        assert_f32_eq(center.into(), 5.666_667);
    }

    /// At an integer scale factor, device-pixel snapping degenerates to
    /// plain logical-pixel rounding -- parity with termy's own
    /// (non-scale-aware) `snapped_stroke_center` at 1x.
    #[test]
    fn stroke_center_snap_matches_logical_rounding_at_1x_scale() {
        let center = snap_center_to_device_pixel(px(0.3), px(11.0), px(1.0), 1.0);
        // center_px = 0.3 + 5.5 = 5.8; edges 5.3/6.3 round to 5.0/6.0.
        assert_f32_eq(center.into(), 5.5);
    }

    #[test]
    fn diagonal_upper_right_to_lower_left_uses_ghostty_style_overshoot() {
        let bounds = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: Size {
                width: px(10.0),
                height: px(20.0),
            },
        };
        let (spec, secondary) =
            diagonal_path_specs(bounds, '\u{2571}', px(1.0)).expect("expected path points");

        assert!(secondary.is_none());
        assert_f32_eq(spec.start.x.into(), 10.25);
        assert_f32_eq(spec.start.y.into(), -0.5);
        assert_f32_eq(spec.end.x.into(), -0.25);
        assert_f32_eq(spec.end.y.into(), 20.5);
    }

    #[test]
    fn diagonal_cross_emits_both_stroked_segments() {
        let bounds = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: Size {
                width: px(10.0),
                height: px(20.0),
            },
        };
        let (primary, secondary) =
            diagonal_path_specs(bounds, '\u{2573}', px(1.0)).expect("expected path points");
        let secondary = secondary.expect("expected second diagonal");

        assert_f32_eq(primary.start.x.into(), 10.25);
        assert_f32_eq(primary.start.y.into(), -0.5);
        assert_f32_eq(secondary.start.x.into(), -0.25);
        assert_f32_eq(secondary.start.y.into(), -0.5);
    }

    #[test]
    fn sextant_char_to_packed_matches_terminal_qr_decoding() {
        assert_eq!(sextant_char_to_packed('\u{1FB00}'), Some(0b01_1111));
        assert_eq!(sextant_char_to_packed('\u{1FB3B}'), Some(0b10_0000));
        assert_eq!(sextant_char_to_packed('\u{2588}'), None);
        assert_eq!(sextant_char_to_packed('\u{1FAFF}'), None);
        assert_eq!(sextant_char_to_packed('\u{1FB3C}'), None);
    }

    #[test]
    fn sextant_geometry_fills_only_the_set_positions() {
        // U+1FB00: only the top-left sub-cell is filled (packed 0b011111
        // means every *other* bit is "empty").
        let geometry = sextant_geometry('\u{1FB00}').expect("expected sextant geometry");
        assert_eq!(geometry.rect_count, 1);
        let rect = geometry.rects()[0];
        assert_eq!(
            (rect.left, rect.top, rect.right, rect.bottom),
            (0.0, 0.0, 0.5, 1.0 / 3.0)
        );
    }

    #[test]
    fn braille_geometry_supports_non_empty_patterns() {
        let geometry = braille_geometry('\u{28FF}').expect("expected braille geometry");
        assert_eq!(geometry.rect_count, 8);
    }

    #[test]
    fn braille_geometry_single_dot_lands_top_left() {
        let geometry = braille_geometry('\u{2801}').expect("expected braille geometry");
        assert_eq!(geometry.rect_count, 1);
        let rect = geometry.rects()[0];
        assert_f32_eq(rect.left, 0.22);
        assert_f32_eq(rect.top, 0.08);
    }

    #[test]
    fn blank_braille_does_not_emit_geometry() {
        assert!(braille_geometry('\u{2800}').is_none());
    }

    #[test]
    fn quad_bounds_are_pixel_snapped() {
        let bounds = Bounds {
            origin: point(px(3.4), px(7.6)),
            size: Size {
                width: px(9.2),
                height: px(10.3),
            },
        };

        let snapped = snapped_cell_bounds(bounds).expect("expected bounds");
        let x: f32 = snapped.origin.x.into();
        let y: f32 = snapped.origin.y.into();
        let width: f32 = snapped.size.width.into();
        let height: f32 = snapped.size.height.into();
        assert_eq!(x.fract(), 0.0);
        assert_eq!(y.fract(), 0.0);
        assert_eq!(width.fract(), 0.0);
        assert_eq!(height.fract(), 0.0);
    }

    #[test]
    fn is_geometric_covers_one_representative_codepoint_per_range() {
        assert!(is_geometric('\u{2500}')); // light horizontal box line
        assert!(is_geometric('\u{2503}')); // heavy vertical box line
        assert!(is_geometric('\u{2551}')); // double vertical box line
        assert!(is_geometric('\u{256D}')); // rounded corner
        assert!(is_geometric('\u{2571}')); // diagonal
        assert!(is_geometric('\u{2584}')); // lower half block
        assert!(is_geometric('\u{1FB00}')); // sextant
        assert!(is_geometric('\u{28FF}')); // braille
        assert!(!is_geometric('\u{2800}')); // blank braille falls back to text
    }
}
