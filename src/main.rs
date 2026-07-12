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
mod sessiond;
mod terminal;
mod terminal_focus;
mod theme;
mod view_chooser;
mod workspace;
mod workspace_state;

use std::io::{self, IsTerminal as _};
use std::process::ExitCode;

use gpui::*;
use gpui_component::{Root, TitleBar};

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

actions!(horizon, [Quit]);

/// Builds the `Application` for this platform, alongside whether that
/// backend already draws its own complete window chrome (used by
/// `WorkspaceShell::new` to skip its own `TitleBar` — see the field doc on
/// `WorkspaceShell::native_decorations`). On Linux this is always
/// `horizon-winit-platform` (real sctk-adwaita CSD decorations — see
/// docs/winit-backend-design.md); every other OS keeps gpui's own platform
/// backend, which relies on Horizon's `TitleBar` for chrome (e.g. macOS's
/// transparent-inset traffic-light layout).
#[cfg(target_os = "linux")]
fn build_application() -> (Application, bool) {
    (
        Application::with_platform(horizon_winit_platform::platform()),
        true,
    )
}

#[cfg(not(target_os = "linux"))]
fn build_application() -> (Application, bool) {
    (gpui_platform::application(), false)
}

fn run_gui() {
    let (application, native_decorations) = build_application();
    // `.with_assets` registers gpui-component's bundled SVGs (icon set,
    // including the titlebar's minimize/maximize/close glyphs) as the
    // window's asset source; without it `Icon`/`IconName` lookups resolve
    // to nothing and the custom titlebar's window controls render blank.
    application
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx| {
            gpui_component::init(cx);
            workspace::init(cx);
            // macOS treats a process with no main menu as owning no menu bar,
            // so the previous app's menu (and name) would linger even with
            // this window focused — installing a minimal menu is what makes
            // Horizon show up as the active application. Activation at launch
            // still needs the explicit activate(true).
            cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.set_menus(vec![Menu {
                name: "Horizon".into(),
                items: vec![MenuItem::action("Quit Horizon", Quit)],
                disabled: false,
            }]);
            cx.activate(true);

            cx.spawn(async move |cx| {
                let ui = &horizon_config::load().ui;
                let size = size(
                    px(ui.window_width.unwrap_or(1100.0) as f32),
                    px(ui.window_height.unwrap_or(720.0) as f32),
                );
                let options = cx.update(|cx| WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
                    // Drive gpui-component's `TitleBar`, rendered as the shell's
                    // first child (see `WorkspaceShell::render`). On macOS this
                    // keeps the native traffic lights (transparent titlebar,
                    // `TitleBar::title_bar_options()` sets the standard inset)
                    // and matches Zed's own window setup. On Linux, GNOME/Mutter
                    // never grants server-side xdg-decoration (it always
                    // negotiates client-side regardless of what's requested), so
                    // without a drawn titlebar the window has no chrome at all;
                    // requesting client decorations explicitly also avoids a
                    // double titlebar on compositors that *do* honor
                    // server-side decoration (e.g. KWin).
                    titlebar: Some(TitleBar::title_bar_options()),
                    window_decorations: Some(WindowDecorations::Client),
                    ..Default::default()
                });
                cx.open_window(options, |window, cx| {
                    // Resolve the socket path before the first pane spawns so
                    // every child process sees HORIZON_SOCKET from the start
                    // (the Floem shell closes the same race in AppState::new).
                    let socket_path = horizon_control::host::socket::default_socket_path();
                    let shell = cx.new(|cx| {
                        WorkspaceShell::new(socket_path.clone(), native_decorations, window, cx)
                    });
                    control_plane::start(
                        shell.downgrade(),
                        window.window_handle(),
                        socket_path,
                        cx,
                    );
                    cx.new(|cx| Root::new(shell, window, cx))
                })
                .expect("Failed to open window");
            })
            .detach();
        });
}
