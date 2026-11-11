//! What a terminal's screen costs to draw by hand on this stack, without a
//! window: `cargo run --release --bin terminal -- /tmp/terminal.png`.
//!
//! First the parts alone (shaping rows, recording the drawing commands,
//! rasterizing them), then the same screen as a widget inside a document,
//! drawn the way a window draws it: Blitz calls the widget's `paint`, takes
//! the commands it recorded into the document's own drawing, and the
//! renderer rasterizes the lot.

use std::time::{Duration, Instant};

use anyrender::ImageRenderer;
use parley::Layout;
use ui_foundation_spike::blitz_traits::shell::{ColorScheme, Viewport};
use ui_foundation_spike::term::{
    Ink, LINE_HEIGHT, Motion, Row, Screen, Shaper, paint_background, paint_cursor, paint_rows,
};
use ui_foundation_spike::{App, el};

fn millis(mut times: Vec<Duration>) -> f64 {
    times.sort();
    times[times.len() / 2].as_secs_f64() * 1e3
}

/// Draws the rows straight into a renderer, with no document around them.
fn alone<R: ImageRenderer>(
    rows: &[Row],
    layouts: &[Layout<Ink>],
    cell_width: f64,
    scale: f64,
    width: u32,
    height: u32,
) -> (f64, f64, Vec<u8>) {
    let mut renderer = R::new(width, height);
    let mut pixels = Vec::new();
    let (mut empties, mut times) = (Vec::new(), Vec::new());
    for _ in 0..21 {
        let started = Instant::now();
        renderer.render_to_vec(
            |scene| paint_background(scene, width as f64, height as f64),
            &mut pixels,
        );
        empties.push(started.elapsed());
    }
    for _ in 0..21 {
        let started = Instant::now();
        renderer.render_to_vec(
            |scene| {
                paint_background(scene, width as f64, height as f64);
                paint_rows(scene, rows, layouts, cell_width, scale, 0.);
                paint_cursor(scene, cell_width, scale, 0.);
            },
            &mut pixels,
        );
        times.push(started.elapsed());
    }
    (millis(times), millis(empties), pixels)
}

/// Draws a document whose whole page is the screen as a widget.
fn in_a_document<R: ImageRenderer>(
    motion: Motion,
    scale: f64,
    width: u32,
    height: u32,
) -> (f64, Vec<u8>) {
    const CSS: &str = "
        html, body { margin: 0; width: 100%; height: 100%; }
        .terminal { width: 100%; height: 100%; }
    ";
    let app = App::new(width, height, CSS, || {
        el("div").class("terminal").widget(Screen::new(motion))
    });
    (app.document().borrow_mut()).set_viewport(Viewport::new(
        width,
        height,
        scale as f32,
        ColorScheme::Light,
    ));
    app.resolve();
    let document = app.document();
    let mut renderer = R::new(width, height);
    let mut pixels = Vec::new();
    let mut times = Vec::new();
    // The first frames make the rows; the ones timed only move them.
    for frame in 0..24 {
        let started = Instant::now();
        app.resolve();
        renderer.render_to_vec(
            |scene| {
                blitz_paint::paint_scene(
                    scene,
                    &mut document.borrow_mut(),
                    scale,
                    width,
                    height,
                    0,
                    0,
                )
            },
            &mut pixels,
        );
        if frame >= 3 {
            times.push(started.elapsed());
        }
    }
    (millis(times), pixels)
}

fn main() {
    let picture = std::env::args().nth(1);
    let mut shaper = Shaper::default();
    println!("milliseconds, median of 21");
    for (columns, lines) in [(80usize, 24usize), (120, 40), (240, 70)] {
        for scale in [1.0f64, 1.5, 2.0] {
            let rows: Vec<Row> = (0..lines)
                .map(|line| Row::dense(columns, line as u64 + 1))
                .collect();
            let (mut all, mut one, mut records) = (Vec::new(), Vec::new(), Vec::new());
            let mut layouts = Vec::new();
            for _ in 0..21 {
                let started = Instant::now();
                layouts = (rows.iter())
                    .map(|row| shaper.shape(row, scale as f32))
                    .collect();
                all.push(started.elapsed());
            }
            for round in 0..21 {
                let changed = Row::dense(columns, 1000 + round);
                let started = Instant::now();
                layouts[lines / 2] = shaper.shape(&changed, scale as f32);
                one.push(started.elapsed());
            }
            let cell_width = shaper.cell_width(scale as f32);
            for _ in 0..21 {
                let started = Instant::now();
                let mut scene = anyrender::Scene::new();
                paint_rows(&mut scene, &rows, &layouts, cell_width, scale, 0.);
                records.push(started.elapsed());
            }
            let (width, height) = (
                (columns as f64 * cell_width).ceil() as u32,
                (lines as f64 * LINE_HEIGHT * scale).ceil() as u32,
            );
            println!("{columns}x{lines} cells at {scale}x ({width}x{height} px)");
            println!(
                "   the parts: shape every row {:6.2} | shape one row {:5.3} | record the commands {:5.2}",
                millis(all),
                millis(one),
                millis(records),
            );
            let (cpu, cpu_empty, _) = alone::<anyrender_vello_cpu::VelloCpuImageRenderer>(
                &rows, &layouts, cell_width, scale, width, height,
            );
            let (gpu, gpu_empty, _) = alone::<anyrender_vello::VelloImageRenderer>(
                &rows, &layouts, cell_width, scale, width, height,
            );
            println!(
                "   drawn alone: GPU {gpu:6.2} (its background alone {gpu_empty:.2}) | CPU {cpu:6.2} (its background alone {cpu_empty:.2})"
            );
            // One more line than the cells, for the widget's own status line.
            let page = height + (LINE_HEIGHT * scale).ceil() as u32;
            let mut pixels = Vec::new();
            for motion in [Motion::Still, Motion::Scroll, Motion::Flood] {
                let (gpu, drawn) = in_a_document::<anyrender_vello::VelloImageRenderer>(
                    motion, scale, width, page,
                );
                let (cpu, _) = in_a_document::<anyrender_vello_cpu::VelloCpuImageRenderer>(
                    motion, scale, width, page,
                );
                println!(
                    "   as a widget in a document, {:6}: GPU {gpu:6.2} | CPU {cpu:6.2}",
                    format!("{motion:?}")
                );
                pixels = drawn;
            }
            if let (Some(path), true) = (&picture, columns == 120 && scale == 1.0) {
                let file = std::fs::File::create(path).expect("the PNG file");
                // The document's page, as the last motion left it.
                let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, page);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header().expect("a PNG header");
                writer.write_image_data(&pixels).expect("PNG data");
                println!("   wrote {path}");
            }
        }
    }
}
