//! Horizon's shell on Blitz.
//!
//! So far one terminal, a session of this shell's own `horizon-terminald`,
//! filling the window in this machine's theme. The workspace and the other
//! views arrive in the order of `docs/ui-foundation-migration-design.md`.
//! The terminal daemon is the root workspace's: build it there first
//! (`cargo build --workspace` at the repository's root).

use std::cell::Cell;

use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};
use horizon_blitz::paths::{daemon_binary, runtime_dir};
use horizon_blitz::terminal::{self, Session};
use horizon_shell_runtime::TerminaldHandle;
use horizon_terminal_core::{
    DEFAULT_SCROLLBACK_LINES, TerminalColorScheme, TerminalSize, TerminalSpawnSpec,
};
use horizon_theme_css::{Theme, themed};
use horizon_ui::{App, Live};

fn main() {
    let sockets = runtime_dir();
    std::fs::create_dir_all(&sockets).expect("the runtime directory");
    let exe = std::env::current_exe().expect("this binary's path");
    if std::env::var_os("HORIZON_TERMINALD_BINARY").is_none()
        && let Some(binary) = daemon_binary("horizon-terminald", &exe)
    {
        // SAFETY: nothing else runs yet; no other thread reads the
        // environment while this writes it.
        unsafe { std::env::set_var("HORIZON_TERMINALD_BINARY", binary) };
    }

    // No control plane yet: the socket a pane's `horizon` command would
    // reach is one of this shell's own that nothing listens on, not the
    // GPUI shell's.
    let control_socket = sockets.join("control.sock");
    let terminald = TerminaldHandle::start(&sockets.join("terminald.sock"), &control_socket);
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let handle = terminald.start_terminal(
        uuid::Uuid::new_v4(),
        TerminalSpawnSpec {
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
            args: Vec::new(),
            term: "xterm-256color".to_string(),
            scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            color_scheme: TerminalColorScheme::default(),
            control_socket,
            fallback_cwd: (std::env::current_dir().ok().or(home)).unwrap_or_else(|| "/".into()),
            spawn_source_session_id: None,
            initial_size: TerminalSize::new(80, 24),
        },
    );

    let look = Theme::of_this_machine();
    let css = [horizon_theme_css::CSS, terminal::CSS].concat();
    let mut shown = None;
    let app = App::new(1100, 720, &css, || {
        let (root, view) = terminal::view(Session::attached(handle), look.terminal());
        shown = Some(view);
        themed(move || look.clone(), root.focus())
    });
    // A frame from the session wakes the window to draw it.
    let (shown, woken) = (shown.expect("the view"), Cell::new(false));
    app.on_poll(move |waker| {
        if !woken.replace(true) {
            shown.set_waker(waker.clone());
        }
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
    event_loop.run_app(application).expect("the event loop");
    // The terminal daemon and its sessions outlive the window, as the
    // GPUI shell's do.
    drop(terminald);
}
