//! The shell over a real `horizon-terminald`, started for the test on a
//! socket of its own: the path the shell's binary takes, end to end (the
//! daemon spawned, a session created over remoc, its frames reaching the
//! view, keys reaching its shell, and the workspace coming back after the
//! shell is closed).
//!
//! The daemon is the root workspace's binary: build it there first
//! (`cargo build --workspace` at the repository's root).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use horizon_blitz::restore;
use horizon_blitz::terminal::{CSS, Session, view};
use horizon_blitz::workspace::{self, Shell};
use horizon_shell_runtime::{TerminaldHandle, wait_for_drain};
use horizon_terminal_core::{
    DEFAULT_SCROLLBACK_LINES, TerminalColorScheme, TerminalSize, TerminalSpawnSpec,
};
use horizon_theme_css::Theme;
use horizon_ui::App;
use horizon_ui::blitz_traits::events::{BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::WithUntracked;
use horizon_workspace::store::WorkspaceStateStore;
use horizon_workspace::{PaneId, SessionId};
use keyboard_types::{Key, Modifiers, NamedKey};

/// A directory for the test's sockets and files, and the daemon in it.
struct Daemon {
    directory: PathBuf,
    socket: PathBuf,
    control_socket: PathBuf,
}

impl Daemon {
    fn prepare() -> Self {
        let daemon =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/horizon-terminald");
        assert!(
            daemon.is_file(),
            "no {}: run `cargo build --workspace` at the repository's root first",
            daemon.display()
        );
        // SAFETY: nextest runs this test in a process of its own, and nothing
        // else in it has started yet.
        unsafe { std::env::set_var("HORIZON_TERMINALD_BINARY", &daemon) };
        // A short path: a socket's is bounded.
        let directory = std::env::temp_dir().join(format!("hzb-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        Self {
            socket: directory.join("terminald.sock"),
            control_socket: directory.join("control.sock"),
            directory,
        }
    }

    /// A client of the daemon, which is started if it is not running.
    fn connect(&self) -> TerminaldHandle {
        TerminaldHandle::start(&self.socket, &self.control_socket)
    }

    fn spec(&self) -> TerminalSpawnSpec {
        TerminalSpawnSpec {
            shell: "sh".to_string(),
            args: Vec::new(),
            term: "xterm-256color".to_string(),
            scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            color_scheme: TerminalColorScheme::default(),
            control_socket: self.control_socket.clone(),
            fallback_cwd: self.directory.clone(),
            spawn_source_session_id: None,
            initial_size: TerminalSize::new(80, 24),
        }
    }

    /// The daemon and its shells end with the test.
    fn stop(&self, terminald: TerminaldHandle) {
        assert!(terminald.begin_reload());
        wait_for_drain(&self.socket).expect("the daemon drained");
        terminald.stop_and_wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn press(app: &App, key: Key, modifiers: Modifiers) {
    let text = match &key {
        Key::Character(text) if modifiers.is_empty() => Some(text.as_str().into()),
        _ => None,
    };
    app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
        key,
        code: keyboard_types::Code::Unidentified,
        modifiers,
        location: keyboard_types::Location::Standard,
        is_auto_repeating: false,
        is_composing: false,
        state: KeyState::Pressed,
        text,
    }));
    app.resolve();
}

/// Types a command into whatever has the focus, and Enter.
fn run(app: &App, command: &str) {
    for character in command.chars() {
        press(
            app,
            Key::Character(character.to_string()),
            Modifiers::default(),
        );
    }
    press(app, Key::Named(NamedKey::Enter), Modifiers::default());
}

/// Lays out (which takes the sessions' frames in) until `met` holds.
fn until(app: &App, what: &str, met: impl Fn() -> bool) {
    let started = Instant::now();
    while !met() {
        assert!(started.elapsed() < Duration::from_secs(20), "no {what}");
        std::thread::sleep(Duration::from_millis(5));
        app.resolve();
    }
}

#[test]
fn a_session_of_a_real_terminald_runs_what_is_typed_into_the_view() {
    let daemon = Daemon::prepare();
    let terminald = daemon.connect();
    let handle = terminald.start_terminal(uuid::Uuid::new_v4(), daemon.spec());

    let mut terminal = None;
    let app = App::new(900, 540, CSS, || {
        let (root, state) = view(
            Session::attached(handle).into(),
            Theme::built_in().terminal(),
        );
        terminal = Some(state);
        root.focus()
    });
    let terminal = terminal.unwrap();
    let wait = |what: &str, met: &dyn Fn(&str) -> bool| {
        until(&app, what, || met(&terminal.text()));
    };
    wait("a prompt", &|text| text.trim_end().ends_with('$'));

    run(&app, "echo daemon-$((6*7))");
    wait("the command's output", &|text| text.contains("daemon-42"));
    // The box's size reached the daemon's PTY.
    let (columns, rows) = terminal.size();
    run(&app, "stty size");
    wait("the size", &|text| {
        text.lines().any(|line| line == format!("{rows} {columns}"))
    });

    daemon.stop(terminald);
}

/// The shell as its binary opens it: over what `state` keeps and the
/// daemon still runs.
fn open(daemon: &Daemon, terminald: &TerminaldHandle, state: &Path) -> (App, Shell) {
    let opening = restore::opening(WorkspaceStateStore::new(state.to_path_buf()), terminald);
    let (spawning, spec) = (terminald.clone(), daemon.spec());
    let spawn: workspace::Spawn = Box::new(move |starting| {
        Session::attached(spawning.start_terminal(starting.session.as_uuid(), spec.clone()))
    });
    let mut shell = None;
    let css = [horizon_theme_css::CSS, &workspace::css()].concat();
    let app = App::new(1200, 800, &css, || {
        let (root, built) = workspace::view(Theme::built_in().terminal(), spawn, opening);
        shell = Some(built);
        horizon_theme_css::themed(Theme::built_in, root)
    });
    app.resolve();
    (app, shell.unwrap())
}

/// The active tab's panes with their sessions, in the order of their
/// places.
fn panes(shell: &Shell) -> Vec<(PaneId, Option<SessionId>)> {
    shell.model.with_untracked(|model| {
        (model.visible_panes().iter())
            .map(|pane| (pane.id, pane.session_id))
            .collect()
    })
}

#[test]
fn the_workspace_comes_back_with_its_sessions_after_the_shell_is_closed() {
    let daemon = Daemon::prepare();
    let state = daemon.directory.join("workspace.json");

    // A first run: something printed, then a second pane.
    let terminald = daemon.connect();
    let (app, shell) = open(&daemon, &terminald, &state);
    let first = panes(&shell);
    assert_eq!(first.len(), 1);
    let text = |shell: &Shell, pane: PaneId| shell.terminal(pane).expect("a terminal").text();
    until(&app, "a prompt", || {
        text(&shell, first[0].0).trim_end().ends_with('$')
    });
    run(&app, "echo first-$((3*3))");
    until(&app, "the output", || {
        text(&shell, first[0].0).contains("first-9")
    });
    press(&app, Key::Character("'".into()), Modifiers::CONTROL);
    press(&app, Key::Character("s".into()), Modifiers::default());
    let kept = panes(&shell);
    assert_eq!(kept.len(), 2);
    assert!(state.is_file());
    until(&app, "the second pane's prompt", || {
        text(&shell, kept[1].0).trim_end().ends_with('$')
    });
    // The shell closes; the daemon and the sessions go on.
    drop(app);
    drop(terminald);

    // A second run finds the same panes over the same sessions, with
    // what the first one printed.
    let terminald = daemon.connect();
    let (app, shell) = open(&daemon, &terminald, &state);
    assert_eq!(panes(&shell), kept);
    until(&app, "what the session showed", || {
        text(&shell, kept[0].0).contains("first-9")
    });
    // The pane that was active has the keys.
    until(&app, "a prompt", || {
        text(&shell, kept[1].0).trim_end().ends_with('$')
    });
    run(&app, "echo second-$((4*4))");
    until(&app, "the output", || {
        text(&shell, kept[1].0).contains("second-16")
    });
    assert!(!text(&shell, kept[0].0).contains("second-16"));

    drop(app);
    daemon.stop(terminald);
}
