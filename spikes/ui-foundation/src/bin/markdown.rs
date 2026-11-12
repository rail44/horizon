//! Horizon's Markdown view on this stack, in a window:
//!
//!     cargo run --release --bin markdown -- path/to/file.md
//!
//! Links to other Markdown files open in place; `j`/`k`, the arrows, space
//! and page keys scroll; backspace, `h` or the left arrow go back. With a
//! second argument it draws the file into that PNG and opens no window.

use std::path::PathBuf;
use std::rc::Rc;

use anyrender::ImageRenderer as _;
use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};
use ui_foundation_spike::blitz_traits::shell::{ColorScheme, Viewport};
use ui_foundation_spike::markdown::{CSS, OpenUrl, view};
use ui_foundation_spike::theme::{self, Theme};
use ui_foundation_spike::{App, Live};

fn main() {
    let mut arguments = std::env::args().skip(1);
    let path = PathBuf::from(arguments.next().expect("a Markdown file"));
    let path = path.canonicalize().unwrap_or(path);
    let picture = arguments.next();

    let open_url: OpenUrl = Rc::new(|url| {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    });
    // The colors, the type scale and the font of this machine's config.
    let look = Theme::of_this_machine();
    let css = [theme::CSS, CSS].concat();
    let app = App::new(900, 700, &css, move || {
        theme::themed(move || look.clone(), view(Some(path), open_url).0.focus())
    });

    if let Some(picture) = picture {
        // At 150 %, as a desktop set to it shows the view.
        let (scale, width, height) = (1.5, 1350, 1050);
        (app.document().borrow_mut()).set_viewport(Viewport::new(
            width,
            height,
            scale,
            ColorScheme::Light,
        ));
        app.resolve();
        let document = app.document();
        let mut renderer = anyrender_vello::VelloImageRenderer::new(width, height);
        let mut pixels = Vec::new();
        renderer.render_to_vec(
            |scene| {
                blitz_paint::paint_scene(
                    scene,
                    &mut document.borrow_mut(),
                    scale as f64,
                    width,
                    height,
                    0,
                    0,
                )
            },
            &mut pixels,
        );
        let file = std::fs::File::create(&picture).expect("the PNG file");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("a PNG header");
        writer.write_image_data(&pixels).expect("PNG data");
        println!("wrote {picture}");
        return;
    }

    blitz_shell::set_after_resolve(ui_foundation_spike::after_resolve);
    let event_loop = create_default_event_loop();
    let (proxy, events) = BlitzShellProxy::new(event_loop.create_proxy());
    let mut application = BlitzApplication::new(proxy, events);
    application.add_window(WindowConfig::new(
        Box::new(Live(app)),
        VelloWindowRenderer::new(),
    ));
    event_loop.run_app(application).expect("the event loop");
}
