//! The GPUI shell binary — see docs/gpui-migration-design.md. M0 hosts a
//! single terminal pane; the workspace tree projection arrives with M2.

mod terminal;
mod theme;

use gpui::*;
use gpui_component::Root;

use crate::terminal::TerminalView;

fn main() {
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);

        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| TerminalView::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}
