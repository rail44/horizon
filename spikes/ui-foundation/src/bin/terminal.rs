//! What a terminal's grid costs to draw by hand on this stack, without a
//! window: `cargo run --release --bin terminal -- /tmp/terminal.png`.
//!
//! A terminal is the content the document does not describe: a widget
//! paints it itself, into the box the document lays out. Each frame it has
//! rows of text in runs of color, some with a background, and a cursor.
//! Here each row is shaped by Parley and drawn as glyph runs and
//! rectangles, which is what a widget's `paint` would record.

use std::time::{Duration, Instant};

use anyrender::{ImageRenderer, PaintScene};
use kurbo::{Affine, Rect};
use parley::{
    FontContext, FontFamilyName, GenericFamily, Layout, LayoutContext, PositionedLayoutItem,
    StyleProperty,
};
use peniko::{Color, Fill};

/// A run's color, as Parley carries it through shaping.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
struct Ink([u8; 3]);

const FONT_SIZE: f32 = 14.;
const LINE_HEIGHT: f64 = 18.;
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
struct Row {
    text: String,
    /// Byte ranges and their colors.
    colors: Vec<(std::ops::Range<usize>, Ink)>,
    /// Column ranges with a background.
    backgrounds: Vec<(std::ops::Range<usize>, [u8; 3])>,
}

/// A row of `columns` cells, different for each `seed`: words of a few
/// characters in changing colors, as a colored listing or an editor shows.
fn row(columns: usize, seed: u64) -> Row {
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

struct Shaper {
    fonts: FontContext,
    layouts: LayoutContext<Ink>,
}

impl Shaper {
    fn shape(&mut self, row: &Row, scale: f32) -> Layout<Ink> {
        let mut builder = self
            .layouts
            .ranged_builder(&mut self.fonts, &row.text, scale, true);
        builder.push_default(FontFamilyName::Generic(GenericFamily::Monospace));
        builder.push_default(StyleProperty::FontSize(FONT_SIZE));
        for (range, ink) in &row.colors {
            builder.push(StyleProperty::Brush(*ink), range.clone());
        }
        let mut layout = builder.build(&row.text);
        layout.break_all_lines(None);
        layout
    }
}

/// Draws the screen: backgrounds, then each row's glyph runs, then a cursor.
fn paint(
    scene: &mut impl PaintScene,
    rows: &[Row],
    layouts: &[Layout<Ink>],
    cell_width: f64,
    scale: f64,
) {
    let line = LINE_HEIGHT * scale;
    for (index, (row, layout)) in rows.iter().zip(layouts).enumerate() {
        let top = index as f64 * line;
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
    let cursor = Rect::new(0., 0., cell_width, line);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(0x35, 0x6d, 0xf3, 0x80),
        None,
        &cursor,
    );
}

fn micros(mut times: Vec<Duration>) -> f64 {
    times.sort();
    times[times.len() / 2].as_secs_f64() * 1e6
}

fn measure<R: ImageRenderer>(
    name: &str,
    rows: &[Row],
    layouts: &[Layout<Ink>],
    cell_width: f64,
    scale: f64,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let mut renderer = R::new(width, height);
    let mut pixels = Vec::new();
    // The same picture with nothing on it but its background: what the
    // renderer and the copy back out of it cost by themselves.
    let mut empties = Vec::new();
    for _ in 0..21 {
        let started = Instant::now();
        renderer.render_to_vec(
            |scene| {
                let screen = Rect::new(0., 0., width as f64, height as f64);
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgb8(0xfd, 0xf6, 0xe3),
                    None,
                    &screen,
                );
            },
            &mut pixels,
        );
        empties.push(started.elapsed());
    }
    let mut times = Vec::new();
    for _ in 0..21 {
        let started = Instant::now();
        renderer.render_to_vec(
            |scene| {
                // The terminal's own background.
                let screen = Rect::new(0., 0., width as f64, height as f64);
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgb8(0xfd, 0xf6, 0xe3),
                    None,
                    &screen,
                );
                paint(scene, rows, layouts, cell_width, scale);
            },
            &mut pixels,
        );
        times.push(started.elapsed());
    }
    println!(
        "      draw and rasterize, {name}: {:9.0} (the background alone: {:.0})",
        micros(times),
        micros(empties)
    );
    pixels
}

fn main() {
    let picture = std::env::args().nth(1);
    let mut shaper = Shaper {
        fonts: FontContext::new(),
        layouts: LayoutContext::new(),
    };
    println!("microseconds, median of 21");
    for (columns, lines) in [(80usize, 24usize), (120, 40), (240, 70)] {
        for scale in [1.0f64, 1.5, 2.0] {
            let rows: Vec<Row> = (0..lines)
                .map(|line| row(columns, line as u64 + 1))
                .collect();

            // Every row shaped: a screen that changed everywhere.
            let mut all = Vec::new();
            let mut layouts = Vec::new();
            for _ in 0..21 {
                let started = Instant::now();
                layouts = rows
                    .iter()
                    .map(|row| shaper.shape(row, scale as f32))
                    .collect();
                all.push(started.elapsed());
            }
            // One row shaped: a screen where one line changed.
            let mut one = Vec::new();
            for round in 0..21 {
                let changed = row(columns, 1000 + round);
                let started = Instant::now();
                layouts[lines / 2] = shaper.shape(&changed, scale as f32);
                one.push(started.elapsed());
            }
            // Recording the drawing commands alone, as a widget's `paint`
            // hands them over.
            let cell_width = layouts[0].width() as f64 / columns as f64;
            let mut records = Vec::new();
            for _ in 0..21 {
                let started = Instant::now();
                let mut scene = anyrender::Scene::new();
                paint(&mut scene, &rows, &layouts, cell_width, scale);
                records.push(started.elapsed());
            }
            let (width, height) = (
                (columns as f64 * cell_width).ceil() as u32,
                (lines as f64 * LINE_HEIGHT * scale).ceil() as u32,
            );
            println!(
                "{columns}x{lines} cells at {scale}x ({width}x{height} px): shape every row {:8.0} | shape one row {:6.0} | record the commands {:7.0}",
                micros(all),
                micros(one),
                micros(records),
            );
            measure::<anyrender_vello_cpu::VelloCpuImageRenderer>(
                "CPU", &rows, &layouts, cell_width, scale, width, height,
            );
            let pixels = measure::<anyrender_vello::VelloImageRenderer>(
                "GPU, read back",
                &rows,
                &layouts,
                cell_width,
                scale,
                width,
                height,
            );
            if let (Some(path), true) = (&picture, columns == 120 && scale == 1.0) {
                let file = std::fs::File::create(path).expect("the PNG file");
                let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header().expect("a PNG header");
                writer.write_image_data(&pixels).expect("PNG data");
                println!("      wrote {path}");
            }
        }
    }
}
