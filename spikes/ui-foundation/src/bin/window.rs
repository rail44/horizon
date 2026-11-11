//! Opens a window on the same layer, to try by hand what a test cannot:
//! typing, the input method, hover, scrolling.
//!
//!     cargo run --release --bin window

use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};
use ui_foundation_spike::demo::{CSS, view};
use ui_foundation_spike::{App, Live};

fn main() {
    let event_loop = create_default_event_loop();
    let (proxy, events) = BlitzShellProxy::new(event_loop.create_proxy());
    let mut application = BlitzApplication::new(proxy, events);
    let app = App::new(900, 640, CSS, view);
    application.add_window(WindowConfig::new(
        Box::new(Live(app)),
        VelloWindowRenderer::new(),
    ));
    event_loop.run_app(application).expect("the event loop");
}
