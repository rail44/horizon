//! The shell's entry point — see docs/gpui-migration-design.md. Any
//! subcommand routes to the control-plane client (`horizon_cli::run`)
//! instead of launching the GUI; with no arguments [`run`] opens the
//! window that hosts the workspace shell (tab strip + recursive splits
//! over the shared `horizon-workspace` model) and starts the control
//! plane on the well-known socket.

use std::io::{self, IsTerminal as _};
use std::process::ExitCode;
use std::sync::Arc;

use gpui::*;
use gpui_component::{Root, TitleBar};

use crate::control_plane;
use crate::theme;
use crate::workspace::{self, WorkspaceShell};

/// The binary's whole body (`src/main.rs`).
pub fn run() -> ExitCode {
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
    let mut stdin = io::stdin();
    let stdin_is_tty = stdin.is_terminal();
    let code = horizon_cli::run(
        args,
        env_socket,
        env_session_id,
        &mut io::stdout(),
        &mut io::stderr(),
        &mut stdin,
        stdin_is_tty,
        &mut horizon_cli::confirm::interactive_prompt,
    );
    ExitCode::from(code)
}

actions!(horizon, [Quit]);

/// Minimal `log` backend: gpui and its platform crates — including the
/// system-notification stack (authorization results, bundle gating,
/// delivery failures, D-Bus absence) — report exclusively through the
/// `log` facade, and with no backend installed every one of those
/// diagnostics is silently dropped. Info cap keeps per-frame chatter out;
/// warnings and errors always land.
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

/// Builds the application with GPUI's maintained native backend for the
/// current OS. The backend owns its event loop, renderer, IME integration,
/// and frame scheduling as one unit.
///
/// The platform is constructed here rather than through
/// `gpui_platform::application()` so its text system can be taken out and
/// handed to preview plugins (`preview::PreviewTextSystem`): gpui exposes
/// the platform text system on the `Platform` object only, and `App` keeps
/// it behind `TextSystem` with no accessor.
fn build_application() -> (Application, Arc<dyn PlatformTextSystem>) {
    let platform = gpui_platform::current_platform(false);
    let text_system = platform.text_system();
    (Application::with_platform(platform), text_system)
}

fn run_gui() {
    // Install the stderr `log` backend before any platform code runs (see
    // `StderrLogger`). The GUI path only; the CLI client keeps its
    // stdout/stderr contract clean.
    let _ = log::set_boxed_logger(Box::new(StderrLogger));
    log::set_max_level(log::LevelFilter::Info);

    let (application, text_system) = build_application();
    // `.with_assets` registers the bundled SVGs (the `gpui-kit-assets` crate,
    // formerly gpui-component-assets), including the client-side titlebar's
    // window-control glyphs.
    application
        .with_assets(gpui_kit_assets::Assets)
        .run(move |cx| {
            gpui_component::init(cx);
            theme::apply_gpui_component_theme(cx);
            cx.set_global(crate::preview::PreviewTextSystem(text_system));
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
