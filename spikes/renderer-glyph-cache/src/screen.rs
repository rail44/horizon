//! A stand-in for a terminal's screen, drawn by hand: rows of text in runs
//! of color, some with a background, and a cursor. The painting is
//! `spikes/ui-foundation/src/term.rs`'s, on anyrender 0.15.

use anyrender::{AntiAliasing, PaintScene};
use kurbo::{Affine, Rect};
use parley::{FontContext, FontFamily, Layout, LayoutContext, PositionedLayoutItem, StyleProperty};
use peniko::{Color, Fill};

/// A run's color, as Parley carries it through shaping.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Ink(pub [u8; 3]);

pub const FONT_SIZE: f32 = 14.;
/// A row's height, in CSS pixels.
pub const LINE_HEIGHT: f64 = 18.;
const BACKGROUND: [u8; 3] = [0xfd, 0xf6, 0xe3];
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

/// What the rows are written in.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Content {
    /// The 90 printing characters of ASCII: few glyphs, each many times.
    Ascii,
    /// Kana and, for three characters in ten, one of two thousand kanji:
    /// many glyphs, each two cells wide.
    Japanese,
}

/// What changes from one frame to the next.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Motion {
    /// Nothing.
    Still,
    /// The rows move up one and a new one comes in at the bottom, as
    /// output scrolling by.
    Scroll,
    /// Every row is new every frame.
    Flood,
}

/// One row of the screen: its text, the color of each run, and the runs
/// that have a background.
pub struct Row {
    text: String,
    /// Byte ranges and their colors.
    colors: Vec<(std::ops::Range<usize>, Ink)>,
    /// Column ranges with a background.
    backgrounds: Vec<(std::ops::Range<usize>, [u8; 3])>,
}

impl Row {
    /// A row of `columns` cells, different for each `seed`: words of a few
    /// characters in changing colors, every cell written, as dense as a
    /// screen gets.
    fn dense(content: Content, columns: usize, seed: u64) -> Self {
        let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let wide = match content {
            Content::Ascii => 1,
            Content::Japanese => 2,
        };
        let mut text = String::new();
        let (mut colors, mut backgrounds) = (Vec::new(), Vec::new());
        let mut column = 0;
        while column + wide <= columns {
            let (start, from) = (text.len(), column);
            let length = (3 + next() % 9) as usize;
            for _ in 0..length {
                if column + wide > columns {
                    break;
                }
                let character = match content {
                    Content::Ascii => (b'!' + (next() % 90) as u8) as char,
                    Content::Japanese if next() % 10 < 3 => {
                        char::from_u32(0x4e00 + (next() % 2000) as u32).unwrap_or('字')
                    }
                    Content::Japanese => {
                        char::from_u32(0x3041 + (next() % 170) as u32).unwrap_or('あ')
                    }
                };
                text.push(character);
                column += wide;
            }
            if column < columns && content == Content::Ascii {
                text.push(' ');
                column += 1;
            }
            let color = PALETTE[(next() % 8) as usize];
            colors.push((start..text.len(), Ink(color)));
            if next() % 12 == 0 {
                backgrounds.push((from..column, [0xee, 0xe8, 0xd5]));
            }
        }
        Row {
            text,
            colors,
            backgrounds,
        }
    }

    fn plain(text: String) -> Self {
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
}

impl Default for Shaper {
    fn default() -> Self {
        Self {
            fonts: FontContext::new(),
            layouts: LayoutContext::new(),
        }
    }
}

impl Shaper {
    fn shape(&mut self, row: &Row, scale: f32) -> Layout<Ink> {
        let mut builder = self
            .layouts
            .ranged_builder(&mut self.fonts, &row.text, scale, true);
        builder.push_default(FontFamily::Source(std::borrow::Cow::Borrowed("monospace")));
        builder.push_default(StyleProperty::FontSize(FONT_SIZE));
        for (range, ink) in &row.colors {
            builder.push(StyleProperty::Brush(*ink), range.clone());
        }
        let mut layout = builder.build(&row.text);
        layout.break_all_lines(None);
        layout
    }

    /// A cell's width in device pixels at `scale`.
    fn cell_width(&mut self, scale: f32) -> f64 {
        let sample = Row::plain("M".repeat(32));
        self.shape(&sample, scale).width() as f64 / 32.
    }
}

/// The rows on screen and their shaping.
pub struct Screen {
    content: Content,
    columns: usize,
    scale: f64,
    cell_width: f64,
    rows: Vec<Row>,
    layouts: Vec<Layout<Ink>>,
    seed: u64,
    /// Draw the background and nothing over it: what a frame costs a
    /// renderer before any of the screen is in it.
    pub blank: bool,
}

impl Screen {
    pub fn new(
        shaper: &mut Shaper,
        content: Content,
        columns: usize,
        lines: usize,
        scale: f64,
    ) -> Self {
        let mut screen = Self {
            content,
            columns,
            scale,
            cell_width: shaper.cell_width(scale as f32),
            rows: Vec::new(),
            layouts: Vec::new(),
            seed: 0,
            blank: false,
        };
        for _ in 0..lines {
            screen.push(shaper);
        }
        screen
    }

    fn push(&mut self, shaper: &mut Shaper) {
        self.seed += 1;
        let row = Row::dense(self.content, self.columns, self.seed);
        self.layouts.push(shaper.shape(&row, self.scale as f32));
        self.rows.push(row);
    }

    /// The screen's size in device pixels.
    pub fn size(&self) -> (u32, u32) {
        (
            (self.columns as f64 * self.cell_width).ceil() as u32,
            (self.rows.len() as f64 * LINE_HEIGHT * self.scale).ceil() as u32,
        )
    }

    /// The next frame's rows.
    pub fn advance(&mut self, shaper: &mut Shaper, motion: Motion) {
        let lines = self.rows.len();
        match motion {
            Motion::Still => {}
            Motion::Scroll => {
                self.rows.remove(0);
                self.layouts.remove(0);
                self.push(shaper);
            }
            Motion::Flood => {
                self.rows.clear();
                self.layouts.clear();
                for _ in 0..lines {
                    self.push(shaper);
                }
            }
        }
    }

    /// Draws the screen: its background, each row's backgrounds and glyph
    /// runs, and a cursor over the first cell.
    pub fn paint(&self, scene: &mut impl PaintScene) {
        let (width, height) = self.size();
        let line = LINE_HEIGHT * self.scale;
        let fill = |scene: &mut _, color: Color, rect: Rect| {
            PaintScene::fill(
                scene,
                Fill::NonZero,
                Affine::IDENTITY,
                color,
                None,
                &rect,
                AntiAliasing::Enabled,
            );
        };
        let [r, g, b] = BACKGROUND;
        fill(
            scene,
            Color::from_rgb8(r, g, b),
            Rect::new(0., 0., width as f64, height as f64),
        );
        if self.blank {
            return;
        }
        for (index, (row, layout)) in self.rows.iter().zip(&self.layouts).enumerate() {
            let top = index as f64 * line;
            for (columns, [r, g, b]) in &row.backgrounds {
                let cells = Rect::new(
                    columns.start as f64 * self.cell_width,
                    top,
                    columns.end as f64 * self.cell_width,
                    top + line,
                );
                fill(scene, Color::from_rgb8(*r, *g, *b), cells);
            }
            let transform = Affine::translate((0., top));
            for item in layout.lines().flat_map(|line| line.items()) {
                let PositionedLayoutItem::GlyphRun(run) = item else {
                    continue;
                };
                let [r, g, b] = run.style().brush.0;
                let shaped = run.run();
                let glyphs: Vec<anyrender::Glyph> = run
                    .positioned_glyphs()
                    .map(|glyph| anyrender::Glyph {
                        id: glyph.id as _,
                        x: glyph.x,
                        y: glyph.y,
                    })
                    .collect();
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
                    glyphs.into_iter(),
                    AntiAliasing::Enabled,
                );
            }
        }
        let cursor = Rect::new(0., 0., self.cell_width, line);
        fill(scene, Color::from_rgba8(0x35, 0x6d, 0xf3, 0x80), cursor);
    }
}
