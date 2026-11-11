//! Shows the hand-drawn terminal screen in a window, as a widget filling a
//! document's page, with its own timings on its first line.
//!
//!     cargo run --release --bin terminal_window -- scroll
//!
//! The argument says what changes each frame: `still` (nothing; it is drawn
//! again when the window asks), `scroll` (the rows move up one and a new
//! one is shaped) or `flood` (every row is new and shaped). The screen has
//! as many cells as fit the window, so resizing it changes the load.

use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};
use ui_foundation_spike::term::{Motion, Screen};
use ui_foundation_spike::{App, Live, el};

const CSS: &str = "
    html, body { margin: 0; width: 100%; height: 100%; }
    .terminal { width: 100%; height: 100%; }
";

fn main() {
    let motion = match std::env::args().nth(1).as_deref() {
        Some("still") => Motion::Still,
        Some("flood") => Motion::Flood,
        Some("scroll") | None => Motion::Scroll,
        Some(other) => panic!("still, scroll or flood, not {other}"),
    };
    let event_loop = create_default_event_loop();
    let (proxy, events) = BlitzShellProxy::new(event_loop.create_proxy());
    let mut application = BlitzApplication::new(proxy, events);
    let app = App::new(1200, 800, CSS, || {
        el("div").class("terminal").widget(Screen::new(motion))
    });
    application.add_window(WindowConfig::new(
        Box::new(Live(app)),
        VelloWindowRenderer::new(),
    ));
    event_loop.run_app(application).expect("the event loop");
}
