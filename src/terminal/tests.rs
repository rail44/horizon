use std::time::{Duration, Instant};

use super::*;
use crate::session::SessionId;

#[test]
fn terminal_intro_mentions_backends() {
    let text = initial_terminal_text();
    assert!(text.contains("portable-pty"));
    assert!(text.contains("alacritty_terminal"));
    assert!(text.contains("termwiz"));
}

#[test]
fn terminal_session_runs_shell_command() {
    let session = TerminalSession::spawn(TerminalSize::new(80, 12), SessionId::new())
        .expect("terminal session should spawn");
    let tx = session.sender();
    let rx = session.updates();

    tx.send(TerminalCommand::Input(
        b"printf horizon-terminal-ok\\n\r".to_vec(),
    ))
    .expect("input should be sent");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_output = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(TerminalUpdate::Snapshot(snapshot)) => {
                if snapshot.text.contains("horizon-terminal-ok") {
                    saw_output = true;
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }

    let _ = tx.send(TerminalCommand::Input(b"exit\r".to_vec()));
    let _ = tx.send(TerminalCommand::Shutdown);

    assert!(saw_output, "terminal session did not render shell output");
}

#[test]
fn terminal_command_sanitizes_emulator_environment() {
    let cmd = terminal_command("/bin/sh", &[], "xterm-kitty", SessionId::new());

    assert_eq!(
        cmd.get_env("TERM").and_then(|v| v.to_str()),
        Some("xterm-kitty")
    );
    assert_eq!(
        cmd.get_env("COLORTERM").and_then(|v| v.to_str()),
        Some("truecolor")
    );
    assert_eq!(
        cmd.get_env("TERM_PROGRAM").and_then(|v| v.to_str()),
        Some("horizon")
    );
    assert_eq!(
        cmd.get_env("TERM_PROGRAM_VERSION").and_then(|v| v.to_str()),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(cmd.get_env("GHOSTTY_RESOURCES_DIR"), None);
    assert_eq!(cmd.get_env("KITTY_LISTEN_ON"), None);
    assert_eq!(cmd.get_env("WEZTERM_PANE"), None);
    assert_eq!(cmd.get_env("TMUX"), None);
}

#[test]
fn terminal_command_injects_the_control_socket_env_var() {
    let cmd = terminal_command("/bin/sh", &[], "xterm-kitty", SessionId::new());

    assert_eq!(
        cmd.get_env("HORIZON_SOCKET"),
        Some(crate::control_plane::default_socket_path().as_os_str())
    );
}

#[test]
fn terminal_command_injects_this_panes_own_session_id() {
    let session_id = SessionId::new();
    let cmd = terminal_command("/bin/sh", &[], "xterm-kitty", session_id);

    assert_eq!(
        cmd.get_env("HORIZON_SESSION_ID").and_then(|v| v.to_str()),
        Some(session_id.as_uuid().to_string().as_str())
    );
}
