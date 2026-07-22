//! The GPUI shell binary — see docs/gpui-migration-design.md. The
//! workspace shell (tab strip + recursive splits over the shared
//! `horizon-workspace` model) hosts a terminal per pane; the control
//! plane listens on the well-known socket. Like the Floem shell's
//! binary, any subcommand routes to the control-plane client
//! (`horizon_cli::run`) instead of launching the GUI.

// `theme.rs`'s `gpui_component_theme_config` builds one large
// `serde_json::json!` object literal (slice B2 grew it past the crate's
// default recursion-limit-driven macro-expansion depth); raising the
// limit is the standard fix for a `json!` macro this size, per
// `serde_json`'s own docs.
#![recursion_limit = "256"]

mod agent;
mod control_plane;
mod input_trace;
mod keymap;
mod palette;
mod session_manager;
mod sessiond;
mod terminal;
mod terminal_focus;
mod theme;
mod theme_settings;
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
/// `horizon_cli::run` stays a pure mapping from arguments to exit code.
fn run_client(args: &[String]) -> ExitCode {
    let env_socket = std::env::var("HORIZON_SOCKET").ok();
    let env_session_id = std::env::var("HORIZON_SESSION_ID").ok();
    let stdin_is_tty = io::stdin().is_terminal();
    let code = horizon_cli::run(
        args,
        env_socket,
        env_session_id,
        &mut io::stdout(),
        &mut io::stderr(),
        stdin_is_tty,
        &mut horizon_cli::confirm::interactive_prompt,
    );
    ExitCode::from(code)
}

actions!(horizon, [Quit]);

/// Builds the application with GPUI's maintained native backend for the
/// current OS. The backend owns its event loop, renderer, IME integration,
/// and frame scheduling as one unit.
fn build_application() -> Application {
    gpui_platform::application()
}

fn run_gui() {
    let application = build_application();
    // `.with_assets` registers gpui-component's bundled SVGs, including the
    // client-side titlebar's window-control glyphs.
    application
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx| {
            gpui_component::init(cx);
            theme::apply_gpui_component_theme(cx);
            workspace::init(cx);
            // macOS treats a process with no main menu as owning no menu bar,
            // so the previous app's menu (and name) would linger even with
            // this window focused — installing a minimal menu is what makes
            // Horizon show up as the active application. Activation at launch
            // still needs the explicit activate(true). GPUI's native platform
            // implements these operations for the current OS.
            cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.set_menus(vec![Menu {
                name: "Horizon".into(),
                items: vec![MenuItem::action("Quit Horizon", Quit)],
                disabled: false,
            }]);
            cx.activate(true);

            cx.spawn(async move |cx| {
                // `[ui] window_width`/`window_height` were retired in the
                // 2026-07-18 config-narrowing wave (see AGENTS.md's
                // "Configuration" section) -- the window now always opens
                // at this fixed size, no file override.
                let size = size(px(1100.0), px(720.0));
                let options = cx.update(|cx| WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
                    // gpui-component renders the matching client-side chrome.
                    // Explicit client decorations avoid a second server-side
                    // titlebar on compositors that support xdg-decoration.
                    titlebar: Some(TitleBar::title_bar_options()),
                    window_decorations: Some(WindowDecorations::Client),
                    ..Default::default()
                });
                cx.open_window(options, |window, cx| {
                    // Resolve the socket path before the first pane spawns so
                    // every child process sees HORIZON_SOCKET from the start
                    // (the Floem shell closes the same race in AppState::new).
                    let socket_path = horizon_control::host::socket::default_socket_path();
                    let shell = cx.new(|cx| WorkspaceShell::new(socket_path.clone(), window, cx));
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
