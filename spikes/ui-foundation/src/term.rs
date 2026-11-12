//! A stand-in for a terminal's screen, drawn by hand: rows of text in runs
//! of color, some with a background, and a cursor.
//!
//! A terminal is content the document does not describe, so a widget
//! paints it into the box the document lays out. Each row is shaped by
//! Parley, kept until that row changes, and drawn as glyph runs and
//! rectangles. The same painting is measured without a window
//! (`src/bin/terminal.rs`) and shown in one (`src/bin/terminal_window.rs`).

use std::time::{Duration, Instant};

use anyrender::PaintScene;
use kurbo::{Affine, Rect};
use parley::{FontContext, FontFamily, Layout, LayoutContext, PositionedLayoutItem, StyleProperty};
use peniko::{Color, Fill};

/// A run's color, as Parley carries it through shaping.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Ink(pub [u8; 3]);

pub const FONT_SIZE: f32 = 14.;
/// A row's height, in CSS pixels.
pub const LINE_HEIGHT: f64 = 18.;
pub const BACKGROUND: [u8; 3] = [0xfd, 0xf6, 0xe3];
const PALETTE: [[u8; 3]; 8] = [
    [0x20, 0x20, 0x20],
    [0xc0, 0x39, 0x2b],
    [0x27, 0xae, 0x60],
    [0xb5, 0x89, 0x00],
    [0x29, 0x80, 0xb9],
    [0x8e, 0x44, 0xad],
    [0x16, 0xa0, 0x85],
    [0x7f, 0x8c, 0x8d],
];

/// One row of the screen: its text, the color of each run, and the runs
/// that have a background.
pub struct Row {
    pub text: String,
    /// Byte ranges and their colors.
    pub colors: Vec<(std::ops::Range<usize>, Ink)>,
    /// Column ranges with a background.
    pub backgrounds: Vec<(std::ops::Range<usize>, [u8; 3])>,
}

impl Row {
    /// A row of `columns` cells, different for each `seed`: words of a few
    /// characters in changing colors, every cell written, as dense as a
    /// screen gets.
    pub fn dense(columns: usize, seed: u64) -> Self {
        let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut text = String::with_capacity(columns);
        let (mut colors, mut backgrounds) = (Vec::new(), Vec::new());
        while text.len() < columns {
            let start = text.len();
            let length = (3 + next() % 9) as usize;
            for _ in 0..length.min(columns - start) {
                text.push((b'!' + (next() % 90) as u8) as char);
            }
            if text.len() < columns {
                text.push(' ');
            }
            let color = PALETTE[(next() % 8) as usize];
            colors.push((start..text.len(), Ink(color)));
            if next() % 12 == 0 {
                backgrounds.push((start..text.len(), [0xee, 0xe8, 0xd5]));
            }
        }
        Row {
            text,
            colors,
            backgrounds,
        }
    }

    /// One line of plain text in one color.
    pub fn plain(text: String) -> Self {
        let colors = vec![(0..text.len(), Ink(PALETTE[0]))];
        Row {
            text,
            colors,
            backgrounds: Vec::new(),
        }
    }
}

pub struct Shaper {
    fonts: FontContext,
    layouts: LayoutContext<Ink>,
    /// The font stack, as CSS writes one, and the size in CSS pixels.
    family: String,
    size: f32,
}

impl Default for Shaper {
    fn default() -> Self {
        Self::with_font("monospace", FONT_SIZE)
    }
}

impl Shaper {
    pub fn with_font(family: &str, size: f32) -> Self {
        Self {
            fonts: FontContext::new(),
            layouts: LayoutContext::new(),
            family: family.to_string(),
            size,
        }
    }

    pub fn shape(&mut self, row: &Row, scale: f32) -> Layout<Ink> {
        let mut builder = self
            .layouts
            .ranged_builder(&mut self.fonts, &row.text, scale, true);
        builder.push_default(FontFamily::Source(std::borrow::Cow::Borrowed(&self.family)));
        builder.push_default(StyleProperty::FontSize(self.size));
        for (range, ink) in &row.colors {
            builder.push(StyleProperty::Brush(*ink), range.clone());
        }
        let mut layout = builder.build(&row.text);
        layout.break_all_lines(None);
        layout
    }

    /// Where a row's baseline is below its top, in device pixels at
    /// `scale`, for a row `line_height` CSS pixels tall: the primary font's
    /// own ascent and descent, centered in the row. A row that falls back
    /// to another font for some characters (Japanese) would otherwise sit
    /// on that font's taller line (`src/terminal/font.rs` does the same).
    pub fn baseline(&mut self, scale: f32, line_height: f64) -> f32 {
        let sample = Row::plain("M".to_string());
        let layout = self.shape(&sample, scale);
        let run = (layout.lines().flat_map(|line| line.items())).find_map(|item| match item {
            PositionedLayoutItem::GlyphRun(run) => Some(run),
            _ => None,
        });
        let Some(run) = run else {
            return 0.;
        };
        let metrics = run.run().font_metrics();
        let padding = (line_height as f32 * scale - metrics.ascent - metrics.descent) / 2.;
        padding + metrics.ascent
    }

    /// A cell's width in device pixels at `scale`.
    pub fn cell_width(&mut self, scale: f32) -> f64 {
        let sample = Row::plain("M".repeat(32));
        self.shape(&sample, scale).width() as f64 / 32.
    }
}

/// Draws rows from `top` down: backgrounds, then each row's glyph runs.
pub fn paint_rows(
    scene: &mut impl PaintScene,
    rows: &[Row],
    layouts: &[Layout<Ink>],
    cell_width: f64,
    scale: f64,
    top: f64,
) {
    let line = LINE_HEIGHT * scale;
    for (index, (row, layout)) in rows.iter().zip(layouts).enumerate() {
        let top = top + index as f64 * line;
        for (columns, color) in &row.backgrounds {
            let cells = Rect::new(
                columns.start as f64 * cell_width,
                top,
                columns.end as f64 * cell_width,
                top + line,
            );
            let [r, g, b] = *color;
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(r, g, b),
                None,
                &cells,
            );
        }
        let transform = Affine::translate((0., top));
        for item in layout.lines().flat_map(|line| line.items()) {
            let PositionedLayoutItem::GlyphRun(run) = item else {
                continue;
            };
            let [r, g, b] = run.style().brush.0;
            let shaped = run.run();
            scene.draw_glyphs(
                shaped.font(),
                shaped.font_size(),
                true,
                &[],
                kurbo::Vec2::default(),
                Fill::NonZero,
                &anyrender::Paint::from(Color::from_rgb8(r, g, b)),
                1.0,
                transform,
                None,
                run.positioned_glyphs().map(|glyph| anyrender::Glyph {
                    id: glyph.id as _,
                    x: glyph.x,
                    y: glyph.y,
                }),
            );
        }
    }
}

/// Fills `width` by `height` with the screen's background.
pub fn paint_background(scene: &mut impl PaintScene, width: f64, height: f64) {
    let [r, g, b] = BACKGROUND;
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(r, g, b),
        None,
        &Rect::new(0., 0., width, height),
    );
}

/// A cursor over the first cell of the rows drawn from `top`.
pub fn paint_cursor(scene: &mut impl PaintScene, cell_width: f64, scale: f64, top: f64) {
    let cursor = Rect::new(0., top, cell_width, top + LINE_HEIGHT * scale);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(0x35, 0x6d, 0xf3, 0x80),
        None,
        &cursor,
    );
}

/// What changes from one frame to the next.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Motion {
    /// Nothing: drawn again only when something else asks.
    Still,
    /// The rows move up one and a new one comes in at the bottom, as output
    /// scrolling by. One row is shaped a frame.
    Scroll,
    /// Every row is new every frame. Every row is shaped a frame.
    Flood,
}

/// A terminal's screen as a widget of the document: it fills its box with
/// as many cells as fit, and draws a line of its own timings above them.
pub struct Screen {
    motion: Motion,
    shaper: Shaper,
    rows: Vec<Row>,
    layouts: Vec<Layout<Ink>>,
    /// The size and scale the rows were made for.
    made_for: (u32, u32, u64),
    cell_width: f64,
    seed: u64,
    last_frame: Option<Instant>,
    /// Recent frames: the time `paint` took, and the time since the frame
    /// before.
    recent: Vec<(Duration, Duration)>,
}

impl Screen {
    pub fn new(motion: Motion) -> Self {
        Self {
            motion,
            shaper: Shaper::default(),
            rows: Vec::new(),
            layouts: Vec::new(),
            made_for: (0, 0, 0),
            cell_width: 0.,
            seed: 0,
            last_frame: None,
            recent: Vec::new(),
        }
    }

    /// The cells the last paint drew.
    pub fn cells(&self) -> (usize, usize) {
        let columns = self.rows.first().map_or(0, |row| row.text.len());
        (columns, self.rows.len())
    }

    fn fresh_row(&mut self, columns: usize) -> Row {
        self.seed += 1;
        Row::dense(columns, self.seed)
    }

    /// The line above the cells: what is drawn, and how long frames take.
    fn status(&self) -> String {
        let (columns, lines) = self.cells();
        let count = self.recent.len().max(1) as f64;
        let mean = |pick: fn(&(Duration, Duration)) -> Duration| {
            self.recent.iter().map(pick).sum::<Duration>().as_secs_f64() * 1000. / count
        };
        let (paint, interval) = (mean(|frame| frame.0), mean(|frame| frame.1));
        let rate = if interval > 0. { 1000. / interval } else { 0. };
        format!(
            "{:?} | {columns}x{lines} cells | widget paint {paint:5.2} ms | frame every {interval:6.2} ms ({rate:5.1} fps)",
            self.motion
        )
    }
}

impl blitz_dom::Widget for Screen {
    fn requires_redraw(&self) -> bool {
        self.motion != Motion::Still
    }

    fn paint(
        &mut self,
        _context: &mut dyn anyrender::RenderContext,
        _styles: &blitz_dom::node::ComputedStyles,
        width: u32,
        height: u32,
        scale: f64,
    ) -> anyrender::Scene {
        let started = Instant::now();
        let line = LINE_HEIGHT * scale;

        // As many cells as fit under the status line.
        if self.made_for != (width, height, scale.to_bits()) {
            self.made_for = (width, height, scale.to_bits());
            self.cell_width = self.shaper.cell_width(scale as f32);
            let columns = ((width as f64 / self.cell_width) as usize).max(1);
            let lines = ((height as f64 / line) as usize).saturating_sub(1);
            self.rows = (0..lines).map(|_| self.fresh_row(columns)).collect();
            let rows = std::mem::take(&mut self.rows);
            self.layouts = (rows.iter())
                .map(|row| self.shaper.shape(row, scale as f32))
                .collect();
            self.rows = rows;
        } else {
            let (columns, lines) = self.cells();
            match self.motion {
                Motion::Still => {}
                Motion::Scroll if lines > 0 => {
                    self.rows.remove(0);
                    self.layouts.remove(0);
                    let row = self.fresh_row(columns);
                    self.layouts.push(self.shaper.shape(&row, scale as f32));
                    self.rows.push(row);
                }
                Motion::Scroll => {}
                Motion::Flood => {
                    for index in 0..lines {
                        let row = self.fresh_row(columns);
                        self.layouts[index] = self.shaper.shape(&row, scale as f32);
                        self.rows[index] = row;
                    }
                }
            }
        }

        let status = Row::plain(self.status());
        let status_layout = self.shaper.shape(&status, scale as f32);
        let mut scene = anyrender::Scene::new();
        paint_background(&mut scene, width as f64, height as f64);
        paint_rows(
            &mut scene,
            std::slice::from_ref(&status),
            std::slice::from_ref(&status_layout),
            self.cell_width,
            scale,
            0.,
        );
        paint_rows(
            &mut scene,
            &self.rows,
            &self.layouts,
            self.cell_width,
            scale,
            line,
        );
        paint_cursor(&mut scene, self.cell_width, scale, line);

        let now = Instant::now();
        let interval = (self.last_frame).map_or(Duration::ZERO, |last| now.duration_since(last));
        self.last_frame = Some(now);
        self.recent.push((started.elapsed(), interval));
        if self.recent.len() > 60 {
            self.recent.remove(0);
        }
        scene
    }
}
