//! Looks, without a window, into two things seen in the window example:
//! where the input method is told the edited text is, and which borders the
//! GPU renderer draws.
//!
//!     cargo run --release --bin look -- /tmp/look

use anyrender::ImageRenderer;
use ui_foundation_spike::blitz_dom::{BaseDocument, LocalName, NodeId, local_name};
use ui_foundation_spike::blitz_traits::events::{BlitzImeEvent, UiEvent};
use ui_foundation_spike::blitz_traits::shell::{ColorScheme, Viewport};
use ui_foundation_spike::demo::{CSS, view};
use ui_foundation_spike::{App, click_at};

fn save<R: ImageRenderer>(app: &App, width: u32, height: u32, scale: f64, path: &str) {
    let document = app.document();
    let mut renderer = R::new(width, height);
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

fn main() {
    let prefix = std::env::args().nth(1).expect("a path prefix for the PNGs");
    for scale in [1.0f32, 1.25, 4.0 / 3.0, 1.5, 5.0 / 3.0, 2.0] {
        // A window of 1200 by 900 logical pixels.
        let (width, height) = ((1200. * scale) as u32, (900. * scale) as u32);
        let app = App::new(width, height, CSS, view);
        (app.document().borrow_mut()).set_viewport(Viewport::new(
            width,
            height,
            scale,
            ColorScheme::Light,
        ));
        app.resolve();
        // Eight rows, as in the window.
        for _ in 0..5 {
            click_at(&app, 60., 80.);
        }
        app.resolve();

        let bounds = |tag: &str| {
            let document = app.document();
            let document = document.borrow();
            let node = (document.find_element_by_tag_name(&LocalName::from(tag))).expect("the tag");
            let rect = document.get_client_bounding_rect(node.id).expect("a box");
            (rect.x, rect.y, rect.width, rect.height)
        };
        for tag in ["input", "textarea"] {
            let (x, y, w, h) = bounds(tag);
            click_at(&app, (x + 20.) as f32, (y + h / 2.) as f32);
            app.resolve();
            let focused = app.ime_cursor_area();
            app.dispatch(UiEvent::Ime(BlitzImeEvent::Preedit(
                "ああああ".to_owned(),
                Some((12, 12)),
            )));
            app.resolve();
            println!(
                "scale {scale}: <{tag}> box x {x:.1} y {y:.1} w {w:.1} h {h:.1} | area on focus {focused:?} | with a preedit {:?}",
                app.ime_cursor_area()
            );
        }
        // What layout gave the first rows' borders: top, right, bottom, left.
        {
            let document = app.document();
            let document = document.borrow();
            let mut borders = Vec::new();
            rows(&document, document.root_element().id, &mut borders);
            for (index, line) in borders.iter().take(4).enumerate() {
                println!("  row {index}: {line}");
            }
        }
        save::<anyrender_vello_cpu::VelloCpuImageRenderer>(
            &app,
            width,
            height,
            scale as f64,
            &format!("{prefix}-cpu-{scale}x.png"),
        );
        save::<anyrender_vello::VelloImageRenderer>(
            &app,
            width,
            height,
            scale as f64,
            &format!("{prefix}-gpu-{scale}x.png"),
        );
    }
}

/// The layout of every element of class `row` under `id`, in order.
fn rows(document: &BaseDocument, id: NodeId, out: &mut Vec<String>) {
    let node = document.get_node(id).expect("a node");
    if node.attr(local_name!("class")) == Some("row") {
        let layout = node.final_layout();
        out.push(format!(
            "y {:.3} h {:.3} border {:.3} {:.3} {:.3} {:.3}",
            layout.location.y,
            layout.size.height,
            layout.border.top,
            layout.border.right,
            layout.border.bottom,
            layout.border.left
        ));
    }
    for child in node.children.iter() {
        rows(document, *child, out);
    }
}
