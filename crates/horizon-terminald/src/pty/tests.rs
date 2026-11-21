use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;

const PATIENCE: Duration = Duration::from_secs(10);

fn size(rows: u16, cols: u16) -> TerminalSize {
    TerminalSize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn sh(script: &str) -> Vec<String> {
    vec!["-c".to_string(), script.to_string()]
}

fn spawn(size: TerminalSize, args: &[String], cwd: &Path) -> (Pty, u32) {
    Pty::spawn(
        size,
        Program {
            path: "/bin/sh",
            args,
            env: Vec::new(),
            cwd,
        },
    )
    .expect("a program in a PTY")
}

/// All the PTY reads until the program and what it started are gone.
fn read_to_end(pty: &Pty) -> String {
    let mut reader = pty.reader().unwrap();
    let (sent, read) = mpsc::channel();
    thread::spawn(move || {
        let mut all = Vec::new();
        let _ = reader.read_to_end(&mut all);
        let _ = sent.send(all);
    });
    let all = read.recv_timeout(PATIENCE).expect("the program ends");
    String::from_utf8_lossy(&all).into_owned()
}

#[test]
fn the_program_reads_its_size_and_its_terminal_from_the_pty() {
    let (pty, _) = spawn(
        size(7, 33),
        &sh("stty size; test -t 0 && echo on-a-terminal; ps -o sid= -p $$; echo $$"),
        Path::new("/"),
    );
    let said = read_to_end(&pty);
    assert!(said.contains("7 33"), "{said:?}");
    assert!(said.contains("on-a-terminal"), "{said:?}");
    // It leads a session of its own: its session id is its own pid.
    let words: Vec<&str> = said.split_whitespace().collect();
    assert_eq!(words[words.len() - 2], words[words.len() - 1], "{said:?}");
}

#[test]
fn a_new_size_reaches_the_program() {
    let (pty, _) = spawn(size(24, 80), &sh("read line; stty size"), Path::new("/"));
    pty.resize(size(40, 120)).unwrap();
    let mut writer = pty.writer().unwrap();
    writer.write_all(b"go\n").unwrap();
    let said = read_to_end(&pty);
    assert!(said.contains("40 120"), "{said:?}");
    drop(writer);
}

#[test]
fn a_descriptor_not_closed_on_exec_is_not_inherited() {
    let mut ends = [0; 2];
    // SAFETY: `pipe` writes two descriptors into the array.
    assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
    // SAFETY: both are open descriptors no one else owns.
    let (read_end, write_end) =
        unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
    let script = format!(
        "test -e /dev/fd/{} && echo inherited; echo checked",
        read_end.as_raw_fd()
    );
    let (pty, _) = spawn(size(24, 80), &sh(&script), Path::new("/"));
    let said = read_to_end(&pty);
    assert!(said.contains("checked"), "{said:?}");
    assert!(!said.contains("inherited"), "{said:?}");
    drop((read_end, write_end));
}

#[test]
fn a_program_that_cannot_be_started_is_an_error() {
    let started = Pty::spawn(
        size(24, 80),
        Program {
            path: "/nonexistent/program",
            args: &[],
            env: Vec::new(),
            cwd: Path::new("/"),
        },
    );
    let error = started.err().expect("no program to start");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

#[test]
fn a_hang_up_ends_the_program() {
    let (pty, pid) = spawn(size(24, 80), &sh("echo ready; exec cat"), Path::new("/"));
    let mut reader = pty.reader().unwrap();
    let mut ready = [0u8; 64];
    let read = reader.read(&mut ready).unwrap();
    assert!(String::from_utf8_lossy(&ready[..read]).contains("ready"));
    hang_up(pid).unwrap();
    read_to_end(&pty);
}

#[test]
fn the_environment_is_the_daemons_with_the_programs_own_over_it() {
    let (pty, _) = Pty::spawn(
        size(24, 80),
        Program {
            path: "/bin/sh",
            args: &sh("echo \"[$HORIZON_PTY_TEST] [$SHELL] [${PATH:+path}]\""),
            env: vec![("HORIZON_PTY_TEST".into(), "set".into())],
            cwd: Path::new("/"),
        },
    )
    .unwrap();
    let said = read_to_end(&pty);
    assert!(said.contains("[set]"), "{said:?}");
    assert!(said.contains("[path]"), "{said:?}");
    assert!(!said.contains("[]"), "a SHELL: {said:?}");
}

#[test]
fn a_directory_that_is_not_there_is_the_home_directory() {
    let home = std::env::var_os("HOME").expect("a HOME");
    let (pty, _) = spawn(
        size(24, 80),
        &sh("pwd -P"),
        Path::new("/nonexistent/directory"),
    );
    let said = read_to_end(&pty);
    let home = std::fs::canonicalize(home).unwrap();
    assert!(said.contains(home.to_str().unwrap()), "{said:?}");
}
