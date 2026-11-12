//! The shell around the views, in a window:
//!
//!     cargo run --release --bin workspace
//!
//! It opens with an agent beside a document over a board, and a terminal in
//! a second tab. Drag a boundary between panes to resize them; drag a row
//! of the board to reorder it; click a pane to give it the keys. `Ctrl+'`
//! enters workspace mode, and `:` there opens the palette: type to narrow
//! it, the arrows choose, Enter runs (`Split Right`, `Split Down`, `Close
//! Pane`, `New Tab`, `Next Tab`, `Show ...`), Escape leaves. With an
//! argument it draws one frame into that PNG and opens no window; a second
//! argument, `palette` (or `plain`), draws it with the palette open, and a
//! third is the display scale (1.5 unless given).

use anyrender::ImageRenderer as _;
use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};
use reactive_graph::traits::Set;
use ui_foundation_spike::blitz_traits::shell::{ColorScheme, Viewport};
use ui_foundation_spike::theme::{self, Theme};
use ui_foundation_spike::workspace::{css, view};
use ui_foundation_spike::{App, Live};

fn main() {
    let mut arguments = std::env::args().skip(1);
    let picture = arguments.next();
    let palette = arguments.next().as_deref() == Some("palette");
    let scale: f32 = (arguments.next())
        .map(|scale| scale.parse().expect("a display scale"))
        .unwrap_or(1.5);

    let mut workspace = None;
    // The colors, the type scale and the font of this machine's config.
    let look = Theme::of_this_machine();
    let css = [theme::CSS, &css()].concat();
    let app = App::new(1200, 800, &css, || {
        let (root, state) = view(look.terminal());
        workspace = Some(state);
        theme::themed(move || look.clone(), root)
    });

    if let Some(picture) = picture {
        let (width, height) = ((1200. * scale) as u32, (800. * scale) as u32);
        (app.document().borrow_mut()).set_viewport(Viewport::new(
            width,
            height,
            scale,
            ColorScheme::Light,
        ));
        if palette {
            // As the keys open it: in workspace mode.
            workspace.as_ref().unwrap().mode.set(true);
            workspace.as_ref().unwrap().palette.set(true);
            app.run_effects();
        }
        app.resolve();
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

    // A terminal's shell printing wakes the window to draw it.
    let workspace = workspace.unwrap();
    let started = std::cell::Cell::new(false);
    app.on_poll(move |waker| {
        if !started.replace(true) {
            workspace.set_waker(waker.clone());
        }
    });
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
