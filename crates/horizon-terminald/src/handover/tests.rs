use std::path::Path;

use super::*;
use crate::pty::Program;

fn a_pty() -> Arc<Pty> {
    let (pty, _) = Pty::spawn(
        TerminalSize::new(20, 5),
        Program {
            path: "/bin/sh",
            args: &["-c".to_string(), "sleep 5".to_string()],
            env: Vec::new(),
            cwd: Path::new("/"),
        },
    )
    .unwrap();
    Arc::new(pty)
}

fn handed(pty: Arc<Pty>) -> Handed {
    Handed {
        session_id: Uuid::new_v4(),
        pty,
        pid: 4242,
        cwd: Some("/tmp/somewhere".into()),
        program_status: None,
        core: HandedCore {
            size: TerminalSize::new(20, 5),
            scrollback_lines: 321,
            color_scheme: TerminalColorScheme::default(),
            state: vec![0, 1, 2, 255],
            unread: b"\x1b]9;half".to_vec(),
        },
    }
}

#[test]
fn what_is_written_is_read_back() {
    let pty = a_pty();
    let one = handed(pty.clone());
    let file = write(std::slice::from_ref(&one)).unwrap();
    let taken = read(file).unwrap();
    assert_eq!(taken.len(), 1);
    let taken = taken.into_iter().next().unwrap();
    assert_eq!(taken.session_id, one.session_id);
    assert_eq!(taken.pid, 4242);
    assert_eq!(taken.cwd, one.cwd);
    assert_eq!(taken.core.size, one.core.size);
    assert_eq!(taken.core.scrollback_lines, 321);
    assert_eq!(taken.core.state, one.core.state);
    assert_eq!(taken.core.unread, one.core.unread);
    // The same open descriptor: the `Pty` still owns it, so the taken
    // owner is let go of without closing it.
    assert_eq!(
        std::os::fd::AsRawFd::as_raw_fd(&taken.master),
        pty.master_fd()
    );
    std::mem::forget(taken.master);
}

#[test]
fn a_session_whose_descriptor_is_not_open_is_left_out() {
    let contents = r#"{"version":1,"sessions":[{"session_id":"7a4c3a0e-3b52-4bd3-9a39-8a3c0f1b2e11","master":987654,"pid":1,"size":{"cols":20,"rows":5,"pixel_width":0,"pixel_height":0}}]}"#;
    let mut file = anonymous_file().unwrap();
    file.write_all(contents.as_bytes()).unwrap();
    file.rewind().unwrap();
    assert!(read(OwnedFd::from(file)).unwrap().is_empty());
}

#[test]
fn a_session_with_only_what_must_be_read_is_read_with_defaults() {
    let pty = a_pty();
    let fd = pty.master().unwrap();
    let number = std::os::fd::IntoRawFd::into_raw_fd(fd);
    let contents = format!(
        r#"{{"version":99,"sessions":[{{"session_id":"7a4c3a0e-3b52-4bd3-9a39-8a3c0f1b2e11","master":{number},"pid":7,"size":{{"cols":20,"rows":5,"pixel_width":0,"pixel_height":0}},"something_new":true}}]}}"#
    );
    let mut file = anonymous_file().unwrap();
    file.write_all(contents.as_bytes()).unwrap();
    file.rewind().unwrap();
    let taken = read(OwnedFd::from(file)).unwrap();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].pid, 7);
    assert!(taken[0].core.state.is_empty());
    assert_eq!(
        taken[0].core.scrollback_lines,
        horizon_terminal_core::DEFAULT_SCROLLBACK_LINES
    );
}
