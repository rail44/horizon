//! The GPUI shell binary — see docs/gpui-migration-design.md. The
//! workspace shell (tab strip + recursive splits over the shared
//! `horizon-workspace` model) hosts a terminal per pane; the control
//! plane listens on the well-known socket. Like the Floem shell's
//! binary, any subcommand routes to the control-plane client
//! (`horizon_ctl::run`) instead of launching the GUI.

mod agent;
mod control_plane;
mod keymap;
mod palette;
mod session_manager;
mod terminal;
mod terminal_focus;
mod theme;
mod workspace;

use std::io::{self, IsTerminal as _};
use std::process::ExitCode;

use gpui::*;
use gpui_component::Root;

use crate::workspace::WorkspaceShell;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        return run_client(&args);
    }
    run_gui();
    ExitCode::SUCCESS
}

/// The control-plane client, exactly like the Floem shell's binary:
/// `HORIZON_SOCKET`/`HORIZON_SESSION_ID` env overrides are read here so
/// `horizon_ctl::run` stays a pure mapping from arguments to exit code.
fn run_client(args: &[String]) -> ExitCode {
    let env_socket = std::env::var("HORIZON_SOCKET").ok();
    let env_session_id = std::env::var("HORIZON_SESSION_ID").ok();
    let stdin_is_tty = io::stdin().is_terminal();
    let code = horizon_ctl::run(
        args,
        env_socket,
        env_session_id,
        &mut io::stdout(),
        &mut io::stderr(),
        stdin_is_tty,
        &mut horizon_ctl::confirm::interactive_prompt,
    );
    ExitCode::from(code)
}

fn run_gui() {
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        workspace::init(cx);
        // Foreground-app activation: without this, clicking the window
        // focuses it but macOS keeps the previous app's name in the menu
        // bar (the process never becomes the active application).
        cx.activate(true);

        cx.spawn(async move |cx| {
            let ui = &horizon_config::load().ui;
            let size = size(
                px(ui.window_width.unwrap_or(1100.0) as f32),
                px(ui.window_height.unwrap_or(720.0) as f32),
            );
            let options = cx.update(|cx| WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
                ..Default::default()
            });
            cx.open_window(options, |window, cx| {
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
