use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use horizon_terminal_core::{
    HandedCore, TerminalColorScheme, TerminalCommand, TerminalCore, TerminalSize,
    TerminalSpawnSpec, TerminalSummary, TerminalUpdate,
};
use uuid::Uuid;

use super::*;
use crate::handover::Taken;
use crate::pty::{Program, Pty};

fn spec() -> TerminalSpawnSpec {
    TerminalSpawnSpec {
        shell: "/bin/sh".into(),
        args: vec!["-i".into()],
        term: "xterm-256color".into(),
        scrollback_lines: 1_000,
        color_scheme: TerminalColorScheme::default(),
        control_socket: "/nonexistent/horizon-control.sock".into(),
        fallback_cwd: std::env::temp_dir(),
        spawn_source_session_id: None,
        initial_size: TerminalSize::new(80, 24),
    }
}

/// The session's screen once it contains `needle`, read as a client reads
/// it: its state asked for, and the bytes after it.
fn screen_until(host: &TerminalHost, session_id: Uuid, needle: &str) -> String {
    let mut channels = host.attach_subscribe(session_id).expect("the session");
    host.handle_command(session_id, TerminalCommand::WantState);
    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut state, mut terminal) = (Vec::new(), None::<TerminalCore>);
    loop {
        if let Some(terminal) = &terminal {
            let text = terminal.snapshot_frame().text();
            if text.contains(needle) {
                return text;
            }
        }
        assert!(
            Instant::now() < deadline,
            "no {needle:?}; last screen: {:?}",
            terminal.map(|terminal| terminal.snapshot_frame().text())
        );
        match channels.events.try_recv() {
            Ok(TerminalUpdate::State { bytes, more }) => {
                state.extend_from_slice(&bytes);
                if !more {
                    terminal = TerminalCore::from_state(&state);
                    state.clear();
                }
            }
            Ok(TerminalUpdate::Output(bytes)) => {
                if let Some(terminal) = terminal.as_mut() {
                    terminal.take_in(&bytes);
                }
            }
            Ok(_) => {}
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn type_in(host: &TerminalHost, session_id: Uuid, text: &[u8]) {
    host.handle_command(session_id, TerminalCommand::Input(text.to_vec()));
}

/// A descriptor to stand for the listening socket.
fn a_descriptor() -> OwnedFd {
    let mut ends = [0; 2];
    // SAFETY: `pipe` writes two descriptors into the array.
    assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
    // SAFETY: both are open descriptors no one else owns; the write end
    // is closed here.
    unsafe {
        drop(OwnedFd::from_raw_fd(ends[1]));
        OwnedFd::from_raw_fd(ends[0])
    }
}

#[test]
fn a_next_image_that_does_not_start_leaves_the_terminals_here() {
    let host = TerminalHost::new();
    let session_id = Uuid::new_v4();
    host.create(session_id, spec()).expect("a terminal");
    // (The words are put together by the shell, so what is typed does not
    // read as what it prints.)
    type_in(
        &host,
        session_id,
        b"KEPT=kept-$((3*3)); printf 'BEFORE_%s\\n' HANDOVER\n",
    );
    screen_until(&host, session_id, "BEFORE_HANDOVER");

    let listener = a_descriptor();
    let next = NextImage {
        executable: "/nonexistent/horizon-terminald".into(),
        socket: "/nonexistent/terminald.sock".into(),
        listener: listener.as_raw_fd(),
        again: None,
    };
    let reason = hand_over(&host, &next);
    assert!(reason.contains("did not start"), "{reason}");

    // The same terminal, its screen, and the same shell, which was not
    // sent an end of file.
    assert_eq!(host.list(), vec![TerminalSummary { session_id }]);
    screen_until(&host, session_id, "BEFORE_HANDOVER");
    type_in(&host, session_id, b"printf 'AFTER_%s\\n' \"$KEPT\"\n");
    screen_until(&host, session_id, "AFTER_kept-9");
    host.shutdown_all();
}

#[test]
fn a_terminal_whose_state_is_not_read_is_asked_to_draw_again() {
    let size = TerminalSize::new(40, 10);
    let script = "trap 'printf \"WINCHED_%s\\n\" ONCE' WINCH; while :; do sleep 0.05; done";
    let (pty, pid) = Pty::spawn(
        size,
        Program {
            path: "/bin/sh",
            args: &["-c".to_string(), script.to_string()],
            env: Vec::new(),
            cwd: Path::new("/"),
        },
    )
    .expect("a program in a PTY");
    // (Its trap is set before the first sleep.)
    thread::sleep(Duration::from_millis(300));
    let session_id = Uuid::new_v4();
    let host = TerminalHost::new();
    host.take(vec![Taken {
        session_id,
        master: pty.master().unwrap(),
        pid,
        cwd: None,
        program_status: None,
        core: HandedCore {
            size,
            scrollback_lines: 100,
            color_scheme: TerminalColorScheme::default(),
            state: b"not a state".to_vec(),
            unread: Vec::new(),
        },
    }]);
    drop(pty);
    screen_until(&host, session_id, "WINCHED_ONCE");
    host.shutdown_all();
}
