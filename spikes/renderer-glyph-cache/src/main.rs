//! What a terminal's screen costs to draw in each of Vello's renderers,
//! with and without the glyph cache, without a window:
//! `cargo run --release` (`-- --pictures <directory>` also writes what
//! each renderer drew, to look at).
//!
//! The screen is the one `spikes/ui-foundation/src/bin/terminal.rs` timed:
//! rows of text in runs of color, some with a background, and a cursor,
//! shaped by Parley and given to the renderer as glyph runs and
//! rectangles. Each renderer draws it off screen and the pixels are read
//! back, so a frame's time is all of the renderer's work on it.

mod screen;
mod targets;

use std::time::{Duration, Instant};

use screen::{Content, Motion, Screen, Shaper};
use targets::{Compute, Cpu, Gpu, Target};

fn millis(mut times: Vec<Duration>) -> f64 {
    times.sort();
    times[times.len() / 2].as_secs_f64() * 1e3
}

/// How many frames a median is of.
const FRAMES: usize = 61;

/// The median time to draw a frame, over frames that change as `motion`
/// says (or, with none, of the background alone), and the last frame's
/// pixels. What a frame takes to shape is not
/// in it: that is the same whoever draws.
fn timed<T: Target>(
    content: Content,
    columns: usize,
    lines: usize,
    scale: f64,
    motion: Option<Motion>,
) -> (f64, Vec<u8>, u32, u32) {
    let mut shaper = Shaper::default();
    let mut screen = Screen::new(&mut shaper, content, columns, lines, scale);
    screen.blank = motion.is_none();
    let (width, height) = screen.size();
    let mut target = T::new(width, height);
    let mut pixels = Vec::new();
    let mut times = Vec::new();
    // The first frames fill whatever the renderer keeps between frames.
    for frame in 0..FRAMES + 3 {
        screen.advance(&mut shaper, motion.unwrap_or(Motion::Still));
        let started = Instant::now();
        target.frame(&screen, &mut pixels);
        if frame >= 3 {
            times.push(started.elapsed());
        }
    }
    (millis(times), pixels, width, height)
}

fn write_png(path: &std::path::Path, pixels: &[u8], width: u32, height: u32) {
    let file = std::fs::File::create(path).expect("the PNG file");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("a PNG header");
    writer.write_image_data(pixels).expect("PNG data");
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let pictures = match arguments.next().as_deref() {
        Some("--pictures") => Some(std::path::PathBuf::from(
            arguments.next().expect("a directory after --pictures"),
        )),
        _ => None,
    };

    println!("GPU: {}", targets::adapter());
    println!("milliseconds a frame, median of {FRAMES}, drawn off screen and read back");
    for content in [Content::Ascii, Content::Japanese] {
        for (columns, lines) in [(120usize, 40usize), (240, 70)] {
            for scale in [1.5f64, 2.0] {
                // What shaping costs, whoever draws.
                let mut shaper = Shaper::default();
                let mut screen = Screen::new(&mut shaper, content, columns, lines, scale);
                let (width, height) = screen.size();
                let (mut all, mut one) = (Vec::new(), Vec::new());
                for _ in 0..FRAMES {
                    let started = Instant::now();
                    screen.advance(&mut shaper, Motion::Flood);
                    all.push(started.elapsed());
                    let started = Instant::now();
                    screen.advance(&mut shaper, Motion::Scroll);
                    one.push(started.elapsed());
                }
                println!(
                    "{content:?} {columns}x{lines} cells at {scale}x ({width}x{height} px): \
                     shaping every row {:.2}, one row {:.3}",
                    millis(all),
                    millis(one),
                );

                macro_rules! row {
                    ($name:literal, $file:literal, $target:ty) => {{
                        let (blank, ..) = timed::<$target>(content, columns, lines, scale, None);
                        let mut cells = vec![format!("background alone {blank:6.2}")];
                        for motion in [Motion::Still, Motion::Scroll, Motion::Flood] {
                            let (time, pixels, width, height) =
                                timed::<$target>(content, columns, lines, scale, Some(motion));
                            cells.push(format!("{motion:?} {time:6.2}"));
                            let shown = motion == Motion::Still && columns == 120 && scale == 1.5;
                            if let (Some(directory), true) = (&pictures, shown) {
                                let name = format!("{content:?}-{}.png", $file).to_lowercase();
                                write_png(&directory.join(name), &pixels, width, height);
                            }
                        }
                        println!("   {:34} {}", $name, cells.join(" | "));
                    }};
                }
                row!("vello (compute), as Horizon has", "compute", Compute);
                row!("vello_gpu", "gpu", Gpu<false>);
                row!("vello_gpu, glyph cache", "gpu-cached", Gpu<true>);
                row!("vello_cpu", "cpu", Cpu<false>);
                row!("vello_cpu, glyph cache", "cpu-cached", Cpu<true>);
            }
        }
    }
}
