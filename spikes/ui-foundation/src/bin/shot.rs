//! Draws a small view without a window, on the CPU, and writes it as a
//! PNG: `cargo run --release --bin shot -- out.png`. It clicks the button
//! twice first, so the picture shows state that came through a signal.

use std::time::Instant;

use anyrender::ImageRenderer as _;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, Update};
use ui_foundation_spike::{App, click_at, el};

const CSS: &str = "
    html { background: #fafafa; }
    body { margin: 0; font-size: 15px; color: #202020; font-family: sans-serif; }
    .column { display: flex; flex-direction: column; gap: 10px; padding: 16px; }
    .title { font-size: 22px; font-weight: 600; }
    .button { align-self: flex-start; padding: 6px 14px; border-radius: 6px;
              background: #356df3; color: white; }
    .row { display: flex; gap: 8px; align-items: center; padding: 4px 8px;
           border: 1px solid #d0d0d0; border-radius: 4px; }
    .label { flex: 1; }
    .muted { color: #808080; }
    .grid { height: 72px; border-radius: 4px; overflow: hidden; }
    input, textarea { font-size: 15px; padding: 4px 6px; border: 1px solid #b0b0b0;
                      border-radius: 4px; }
";

/// A stand-in for a terminal: a grid of cells painted by hand, in a box
/// the document lays out.
struct Grid;

impl ui_foundation_spike::blitz_dom::Widget for Grid {
    fn paint(
        &mut self,
        _context: &mut dyn anyrender::RenderContext,
        _styles: &ui_foundation_spike::blitz_dom::node::ComputedStyles,
        width: u32,
        height: u32,
        _scale: f64,
    ) -> anyrender::Scene {
        use anyrender::PaintScene as _;
        use kurbo::{Affine, Rect};
        use peniko::{Color, Fill};

        let mut scene = anyrender::Scene::new();
        let (cell_width, cell_height) = (9.0, 18.0);
        let columns = (width as f64 / cell_width) as u32;
        let rows = (height as f64 / cell_height) as u32;
        for row in 0..rows {
            for column in 0..columns {
                let shade = (((row * 31 + column * 17) % 9) * 20 + 40) as u8;
                let cell = Rect::new(
                    column as f64 * cell_width,
                    row as f64 * cell_height,
                    (column + 1) as f64 * cell_width - 1.0,
                    (row + 1) as f64 * cell_height - 1.0,
                );
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgb8(shade, shade + 20, 60),
                    None,
                    &cell,
                );
            }
        }
        scene
    }
}

const WIDTH: u32 = 640;
const HEIGHT: u32 = 420;

fn main() {
    let path = std::env::args().nth(1).expect("a path for the PNG");
    let count = RwSignal::new(0u32);
    let app = App::new(WIDTH, HEIGHT, CSS, || {
        el("div")
            .class("column")
            .child(el("div").class("title").text("signal から blitz-dom へ"))
            .child(
                el("div")
                    .class("button")
                    .on("click", move |_| count.update(|count| *count += 1))
                    .text_dyn(move || format!("{} clicks", count.get())),
            )
            .each(
                move || (0..count.get() + 1).collect::<Vec<u32>>(),
                |row| *row,
                |row| {
                    el("div")
                        .class("row")
                        .child(el("span").class("label").text(&format!("行 {row}")))
                        .child(el("span").class("muted").text("remove"))
                },
            )
            .child(
                el("input")
                    .attr("type", "text")
                    .attr("value", "入力欄 input"),
            )
            .child(el("textarea").text("テキストエリア\ntextarea"))
            .child(el("div").class("grid").widget(Grid))
    });
    app.resolve();
    // Over the button, as laid out: below the title.
    click_at(&app, 30., 64.);
    click_at(&app, 30., 64.);
    app.resolve();

    let document = app.document();
    let mut renderer = VelloCpuImageRenderer::new(WIDTH, HEIGHT);
    // Widgets paint once a renderer is there for them.
    document.borrow_mut().can_create_surfaces(&mut renderer);
    let mut pixels = Vec::new();
    let started = Instant::now();
    renderer.render_to_vec(
        |scene| {
            blitz_paint::paint_scene(scene, &mut document.borrow_mut(), 1.0, WIDTH, HEIGHT, 0, 0)
        },
        &mut pixels,
    );
    println!("painted and rasterized in {:?}", started.elapsed());

    let file = std::fs::File::create(&path).expect("the PNG file");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), WIDTH, HEIGHT);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("a PNG header");
    writer.write_image_data(&pixels).expect("PNG data");
    println!("wrote {path}");
}
