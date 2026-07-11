//! The GPUI shell binary — see docs/gpui-migration-design.md. The
//! workspace shell (tab strip + recursive splits over the shared
//! `horizon-workspace` model) hosts a terminal per pane; the control
//! plane listens on the well-known socket.

mod agent;
mod control_plane;
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
                // Resolve the socket path before the first pane spawns so
                // every child process sees HORIZON_SOCKET from the start
                // (the Floem shell closes the same race in AppState::new).
                let socket_path = horizon_control::host::socket::default_socket_path();
                let shell = cx.new(|cx| WorkspaceShell::new(socket_path.clone(), window, cx));
                control_plane::start(shell.downgrade(), window.window_handle(), socket_path, cx);
                cx.new(|cx| Root::new(shell, window, cx))
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}
