//! Horizon's shell on Blitz.
//!
//! So far a window in this machine's theme, with nothing in it: the
//! terminal, the workspace and the other views arrive in the order of
//! `docs/ui-foundation-migration-design.md`.

use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};
use horizon_theme_css::{Theme, themed};
use horizon_ui::{App, Live, el};

const CSS: &str = "
    .empty { display: flex; align-items: center; justify-content: center; height: 100%;
             color: var(--text-muted); }
";

fn main() {
    let look = Theme::of_this_machine();
    let css = [horizon_theme_css::CSS, CSS].concat();
    let app = App::new(1100, 720, &css, || {
        themed(
            move || look.clone(),
            el("div").class("empty").text("Horizon"),
        )
    });

    // The layer lays the document out for each frame.
    blitz_shell::set_frame(horizon_ui::frame);
    let event_loop = create_default_event_loop();
    let (proxy, events) = BlitzShellProxy::new(event_loop.create_proxy());
    let mut application = BlitzApplication::new(proxy, events);
    application.add_window(WindowConfig::new(
        Box::new(Live(app)),
        VelloWindowRenderer::new(),
    ));
    event_loop.run_app(application).expect("the event loop");
}
