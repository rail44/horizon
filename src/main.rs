//! Horizon: the shell's window, or with arguments the control plane's
//! client (`horizon <subcommand>`, `horizon_cli::run`).
//!
//! The window is the workspace over this machine's two runtimes,
//! `horizon-terminald` and `horizon-agentd`, each started if it is not
//! running, in this machine's theme, with the workspace kept between runs.

use std::cell::Cell;
use std::io::{self, IsTerminal as _};
use std::process::ExitCode;

use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};
use horizon::agentd::{Agentd, RuntimeModels};
use horizon::terminal::Session;
use horizon::terminald::Terminald;
use horizon::{control, desktop_notify, restore, window, workspace};
use horizon_terminal_core::{
    DEFAULT_SCROLLBACK_LINES, TerminalColorScheme, TerminalSize, TerminalSpawnSpec,
};
use horizon_ui::{App, Live};
use horizon_workspace::store::WorkspaceStateStore;

/// Hands a URL to the desktop.
fn open_url(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(error) = std::process::Command::new(opener).arg(url).spawn() {
        eprintln!("could not open {url} with {opener}: {error}");
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        return run_client(&args);
    }
    run_window();
    ExitCode::SUCCESS
}

/// The control plane's client. `HORIZON_SOCKET` and `HORIZON_SESSION_ID`
/// are read here, so that `horizon_cli::run` stays a mapping from
/// arguments to an exit code.
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

fn run_window() {
    // The socket a pane's `horizon` command reaches.
    let control_socket = horizon_control::host::socket::default_socket_path();
    let requests = control::listen(control_socket.clone());
    let terminald = Terminald::start(
        &horizon_wire::socket::default_terminald_socket_path(),
        &control_socket,
    );
    let agentd = Agentd::start(
        &horizon_wire::socket::default_agentd_socket_path(),
        &control_socket,
    );
    let store = WorkspaceStateStore::from_environment();
    let wake = horizon::wake::Wake::default();
    let (poster, clicked) = desktop_notify::poster(wake.clone());
    let opening = workspace::Opening {
        notify: Some(std::rc::Rc::new(move |session, notification| {
            poster.post(session, notification.title, &notification.body);
        })),
        config: horizon_config::load().clone(),
        reload: Some(Box::new(horizon_config::reload)),
        agents: Some(agentd.spawn()),
        open_url: Some(std::rc::Rc::new(open_url)),
        models: Some(std::sync::Arc::new(RuntimeModels(agentd.clone()))),
        agent_runtime: Some(agentd.runtime()),
        reload_terminals: Some(terminald.reload()),
        ..restore::opening(store, &terminald, Some(&agentd))
    };
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let directory = (std::env::current_dir().ok().or(home)).unwrap_or_else(|| "/".into());
    let spawn: workspace::Spawn = Box::new(move |starting| {
        Session::attached(terminald.handle().start_terminal(
            starting.session.as_uuid(),
            TerminalSpawnSpec {
                shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
                args: Vec::new(),
                term: "xterm-256color".to_string(),
                scrollback_lines: DEFAULT_SCROLLBACK_LINES,
                color_scheme: TerminalColorScheme::default(),
                control_socket: control_socket.clone(),
                fallback_cwd: starting.directory.unwrap_or_else(|| directory.clone()),
                spawn_source_session_id: starting.from.map(|from| from.as_uuid()),
                initial_size: TerminalSize::new(80, 24),
            },
        ))
    });

    let mut shell = None;
    let app = App::new(1100, 720, &workspace::css(), || {
        let (root, built) = workspace::view(spawn, opening);
        shell = Some(built);
        root
    });
    // A frame from a session wakes the window to draw it, and so does
    // something asked over the control plane or by the agent daemon,
    // which is answered here.
    let (shell, woken) = (shell.expect("the shell"), Cell::new(false));
    app.on_poll(move |waker| {
        if !woken.replace(true) {
            shell.set_waker(waker.clone());
            wake.set(waker.clone());
            window::set_waker(waker.clone());
        }
        for session in clicked.take() {
            shell.reveal_session(session);
        }
        requests.answer(waker, &shell);
        agentd.answer(waker, &shell);
    });

    // `horizon-ui` lays the document out for each frame.
    blitz_shell::set_frame(horizon_ui::frame);
    blitz_shell::set_window_focused(window::set_active);
    horizon::primary::connect(
        blitz_shell::set_primary_selection_text,
        blitz_shell::request_primary_selection_text,
    );
    let event_loop = create_default_event_loop();
    let (proxy, events) = BlitzShellProxy::new(event_loop.create_proxy());
    let mut application = BlitzApplication::new(proxy, events);
    application.add_window(WindowConfig::new(
        Box::new(Live(app)),
        VelloWindowRenderer::new(),
    ));
    // The daemons and their sessions outlive the window.
    event_loop.run_app(application).expect("the event loop");
}
