//! Horizon's shell on Blitz.
//!
//! So far the workspace with terminals and agents in its panes, sessions
//! of this shell's own `horizon-terminald` and `horizon-agentd`, in this
//! machine's theme, kept between runs in a file of this shell's own. The other
//! views arrive in the order of `docs/ui-foundation-migration-design.md`.
//! The daemons are the root workspace's: build them there first
//! (`cargo build --workspace` at the repository's root).

use std::cell::Cell;

use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};
use horizon_blitz::agentd::{Agentd, RuntimeModels};
use horizon_blitz::paths::{daemon_binary, data_home, runtime_dir, workspace_state};
use horizon_blitz::terminal::Session;
use horizon_blitz::terminald::Terminald;
use horizon_blitz::{control, restore, workspace};
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

fn main() {
    let sockets = runtime_dir();
    std::fs::create_dir_all(&sockets).expect("the runtime directory");
    let exe = std::env::current_exe().expect("this binary's path");
    // What the daemons this shell starts inherit: their binaries, and
    // places of their own for what they keep. The agent sessions' log and
    // projection are under the data home unless named, so they are not
    // named.
    // SAFETY: nothing else runs yet; no other thread reads the environment
    // while this writes it.
    unsafe {
        for (name, variable) in [
            ("horizon-terminald", "HORIZON_TERMINALD_BINARY"),
            ("horizon-agentd", "HORIZON_AGENTD_BINARY"),
            ("horizon-logd", "HORIZON_LOGD_BINARY"),
        ] {
            if std::env::var_os(variable).is_none()
                && let Some(binary) = daemon_binary(name, &exe)
            {
                std::env::set_var(variable, binary);
            }
        }
        std::env::set_var("HORIZON_DATA_HOME", data_home());
        std::env::set_var("HORIZON_LOGD_SOCKET", sockets.join("logd.sock"));
        std::env::remove_var("HORIZON_AGENT_EVENT_LOG");
        std::env::remove_var("HORIZON_AGENT_STATE_DB");
    }

    // The socket a pane's `horizon` command reaches is this shell's own,
    // not the GPUI shell's.
    let control_socket = sockets.join("control.sock");
    let requests = control::listen(control_socket.clone());
    let terminald = Terminald::start(&sockets.join("terminald.sock"), &control_socket);
    let agentd = Agentd::start(&sockets.join("agentd.sock"), &control_socket);
    let store = WorkspaceStateStore::new(workspace_state());
    let opening = workspace::Opening {
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
        }
        requests.answer(waker, &shell);
        agentd.answer(waker, &shell);
    });

    // The layer lays the document out for each frame.
    blitz_shell::set_frame(horizon_ui::frame);
    let event_loop = create_default_event_loop();
    let (proxy, events) = BlitzShellProxy::new(event_loop.create_proxy());
    let mut application = BlitzApplication::new(proxy, events);
    application.add_window(WindowConfig::new(
        Box::new(Live(app)),
        VelloWindowRenderer::new(),
    ));
    // The terminal daemon and its sessions outlive the window, as the
    // GPUI shell's do.
    event_loop.run_app(application).expect("the event loop");
}
