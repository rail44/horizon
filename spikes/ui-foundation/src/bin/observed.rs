//! Reproduces, without a window, two things seen in the window example:
//! a click above a scrolled list not reaching what is drawn there, and
//! borders drawn on some sides only.
//!
//!     cargo run --release --bin observed -- /tmp/observed

use anyrender::ImageRenderer as _;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, Update};
use ui_foundation_spike::blitz_dom::ScrollBehavior;
use ui_foundation_spike::blitz_traits::events::{
    BlitzPointerEvent, BlitzPointerId, MouseEventButton, MouseEventButtons, PointerCoords, UiEvent,
};
use ui_foundation_spike::{App, El, click_at, el};

const CSS: &str = "
    html { background: #fafafa; }
    body { margin: 0; font-size: 15px; color: #202020; font-family: sans-serif; }
    .column { display: flex; flex-direction: column; gap: 10px; padding: 16px; }
    .button { align-self: flex-start; padding: 6px 14px; border-radius: 6px;
              background: #356df3; color: white; }
    .row { display: flex; gap: 8px; align-items: center; padding: 4px 8px;
           border: 1px solid #d0d0d0; border-radius: 4px; }
    .row:hover { background: #f0f0f0; }
    .list { display: flex; flex-direction: column; gap: 4px; height: 100px;
            overflow-y: scroll; }
    input, textarea { font-size: 15px; padding: 6px 8px; border: 1px solid #b0b0b0;
                      border-radius: 4px; line-height: 1.5; }
    textarea { height: 60px; }
";

const WIDTH: u32 = 480;
const HEIGHT: u32 = 360;

fn save(app: &App, scale: f64, path: &str) {
    let (width, height) = (
        (WIDTH as f64 * scale) as u32,
        (HEIGHT as f64 * scale) as u32,
    );
    let document = app.document();
    let mut renderer = VelloCpuImageRenderer::new(width, height);
    let mut pixels = Vec::new();
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
    let file = std::fs::File::create(path).expect("the PNG file");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("a PNG header");
    writer.write_image_data(&pixels).expect("PNG data");
    println!("wrote {path}");
}

fn move_to(app: &App, x: f32, y: f32) {
    app.dispatch(UiEvent::PointerMove(BlitzPointerEvent {
        id: BlitzPointerId::Mouse,
        is_primary: true,
        coords: PointerCoords {
            page_x: x,
            page_y: y,
            screen_x: x,
            screen_y: y,
            client_x: x,
            client_y: y,
        },
        button: MouseEventButton::Main,
        buttons: MouseEventButtons::None,
        mods: Default::default(),
        details: Default::default(),
        element: Default::default(),
        active_pointers: Default::default(),
    }));
}

fn main() {
    let prefix = std::env::args().nth(1).expect("a path prefix for the PNGs");
    let (button_clicks, row_clicks) = (RwSignal::new(0u32), RwSignal::new(0u32));
    let mut list: Option<El> = None;
    let app = App::new(WIDTH, HEIGHT, CSS, || {
        let rows = el("div").class("list").each(
            || (0..12u32).collect::<Vec<_>>(),
            |row| *row,
            move |row| {
                el("div")
                    .class("row")
                    .on("click", move |_| row_clicks.update(|count| *count += 1))
                    .text(&format!("row {row}"))
            },
        );
        list = Some(rows);
        el("div")
            .class("column")
            .child(
                el("div")
                    .class("button")
                    .on("click", move |_| button_clicks.update(|count| *count += 1))
                    .text("button"),
            )
            .child(el("input").attr("type", "text").attr("value", "input"))
            .child(el("textarea").text("textarea"))
            .child(rows)
    });
    let list = list.unwrap();
    app.resolve();

    // 1. A click on the button, before and after the list below it is
    //    scrolled so that its first rows are above its top edge.
    click_at(&app, 30., 30.);
    println!(
        "before scrolling: button {} row {}",
        button_clicks.get(),
        row_clicks.get()
    );
    (app.document().borrow_mut()).scroll_to(list.0, 0., 200., ScrollBehavior::Instant);
    app.resolve();
    click_at(&app, 30., 30.);
    println!(
        "after scrolling the list by 200: button {} row {}",
        button_clicks.get(),
        row_clicks.get()
    );

    // 2. Borders, at two scales, at rest and with the pointer over a row
    //    and the input focused.
    save(&app, 1.0, &format!("{prefix}-rest-1x.png"));
    save(&app, 2.0, &format!("{prefix}-rest-2x.png"));
    click_at(&app, 60., 70.);
    move_to(&app, 60., 240.);
    app.resolve();
    save(&app, 2.0, &format!("{prefix}-hover-focus-2x.png"));
}
