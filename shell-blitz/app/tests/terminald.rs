//! The terminal view over a real `horizon-terminald`, started for the test
//! on a socket of its own: the path the shell's binary takes, end to end
//! (the daemon spawned, a session created over remoc, its frames reaching
//! the view, keys reaching its shell).
//!
//! The daemon is the root workspace's binary: build it there first
//! (`cargo build --workspace` at the repository's root).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use horizon_blitz::terminal::{CSS, Session, view};
use horizon_shell_runtime::{TerminaldHandle, wait_for_drain};
use horizon_terminal_core::{
    DEFAULT_SCROLLBACK_LINES, TerminalColorScheme, TerminalSize, TerminalSpawnSpec,
};
use horizon_theme_css::Theme;
use horizon_ui::App;
use horizon_ui::blitz_traits::events::{BlitzKeyEvent, KeyState, UiEvent};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_session_of_a_real_terminald_runs_what_is_typed_into_the_view() {
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
    let scratch = Scratch(std::env::temp_dir().join(format!("hzb-{}", std::process::id())));
    std::fs::create_dir_all(&scratch.0).unwrap();
    let socket = scratch.0.join("terminald.sock");
    let control_socket = scratch.0.join("control.sock");

    let terminald = TerminaldHandle::start(&socket, &control_socket);
    let handle = terminald.start_terminal(
        uuid::Uuid::new_v4(),
        TerminalSpawnSpec {
            shell: "sh".to_string(),
            args: Vec::new(),
            term: "xterm-256color".to_string(),
            scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            color_scheme: TerminalColorScheme::default(),
            control_socket,
            fallback_cwd: scratch.0.clone(),
            spawn_source_session_id: None,
            initial_size: TerminalSize::new(80, 24),
        },
    );

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
        let started = Instant::now();
        loop {
            app.resolve();
            let text = terminal.text();
            if met(&text) {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "no {what} in:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    wait("a prompt", &|text| text.trim_end().ends_with('$'));

    let key = |key: keyboard_types::Key| {
        let text = match &key {
            keyboard_types::Key::Character(text) => Some(text.as_str().into()),
            _ => None,
        };
        app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
            key,
            code: keyboard_types::Code::Unidentified,
            modifiers: Default::default(),
            location: keyboard_types::Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
            text,
        }));
    };
    for character in "echo daemon-$((6*7))".chars() {
        key(keyboard_types::Key::Character(character.to_string()));
    }
    key(keyboard_types::Key::Named(keyboard_types::NamedKey::Enter));
    wait("the command's output", &|text| text.contains("daemon-42"));
    // The box's size reached the daemon's PTY.
    let (columns, rows) = terminal.size();
    for character in "stty size".chars() {
        key(keyboard_types::Key::Character(character.to_string()));
    }
    key(keyboard_types::Key::Named(keyboard_types::NamedKey::Enter));
    wait("the size", &|text| {
        text.lines().any(|line| line == format!("{rows} {columns}"))
    });

    // The daemon and its shell end with the test.
    assert!(terminald.begin_reload());
    wait_for_drain(&socket).expect("the daemon drained");
    terminald.stop_and_wait();
}
