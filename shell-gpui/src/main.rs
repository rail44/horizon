//! The GPUI shell binary — see docs/gpui-migration-design.md. M2: the
//! workspace shell (tab strip + recursive splits over the shared
//! `horizon-workspace` model), each pane hosting a terminal.

mod palette;
mod session_manager;
mod terminal;
mod theme;
mod workspace;

use gpui::*;
use gpui_component::Root;

use crate::workspace::WorkspaceShell;

fn main() {
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        workspace::init(cx);

        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let shell = cx.new(|cx| WorkspaceShell::new(window, cx));
                cx.new(|cx| Root::new(shell, window, cx))
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}
