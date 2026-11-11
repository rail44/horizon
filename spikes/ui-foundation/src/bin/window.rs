//! Opens a window on the same layer, to try by hand what a test cannot:
//! typing, the input method, hover, scrolling.
//!
//!     cargo run --release --bin window

use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};
use ui_foundation_spike::App;
use ui_foundation_spike::blitz_dom::{DocGuard, DocGuardMut, Document};
use ui_foundation_spike::blitz_traits::events::UiEvent;
use ui_foundation_spike::demo::{CSS, view};

/// The layer's document as the shell's: input goes through the layer's
/// handlers, and a turn of the shell runs the signals' effects.
struct Live(App);

impl Document for Live {
    fn inner(&self) -> DocGuard<'_> {
        DocGuard::RefCell(self.0.document_cell().borrow())
    }

    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        DocGuardMut::RefCell(self.0.document_cell().borrow_mut())
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        self.0.dispatch(event);
    }
}

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
