//! Research-only latency probe for the 2026-07-18 dogfooding investigation
//! ("keyboard input latency in a Horizon terminal pane running Claude
//! Code"). NOT part of the product test surface: every test here is
//! `#[ignore]`d so `cargo nextest run --workspace` never runs them, and
//! they print a median/p95 report to stdout rather than asserting a
//! threshold (there is no product requirement to gate on yet -- this file
//! exists to attribute where the time goes, not to enforce a budget).
//!
//! Measures the wire-level round trip -- `TerminalCommand` sent over the
//! real Unix socket to a real `horizon-sessiond`, to `TerminalUpdate`
//! arriving back -- for: a plain shell's kernel-level PTY echo (the floor),
//! a synthetic TUI that brackets redraws in DEC private mode 2026
//! (BSU/ESU) under several sub-variants (well-behaved single write,
//! multi-chunk, delayed-ESU, never-closed), and a bare DECRQM(2026)
//! capability probe matching the pattern real ink/Claude-Code-style apps
//! use to detect synchronized-output support before ever opening a window.
//!
//! This intentionally reuses `tests/e2e.rs`'s spawn/handshake/wire-helper
//! shapes rather than importing them (a `tests/*.rs` file is its own
//! binary crate; sharing would need a `tests/common/mod.rs` restructure
//! this throwaway investigation file isn't worth forcing on that suite).
//!
//! Run explicitly, e.g.:
//! ```sh
//! cargo test -p horizon-sessiond --test latency_probe -- --ignored --nocapture --test-threads=1
//! ```

use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use horizon_session_protocol::{self as session_wire, Hello, SessionControl, SESSION_CONTROL_KIND};
use horizon_terminal_core::{
    apply_frame_diff, decode_terminal_update, encode_terminal_command, encode_terminal_control,
    KeyEventKind, TerminalColorScheme, TerminalCommand, TerminalControl, TerminalSize,
    TerminalSpawnSpec, TerminalUpdate,
};
use termwiz::input::{KeyCode, Modifiers};
use tokio::io::BufReader;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

const TRANSIENT_LINK_RETRY_DELAY: Duration = Duration::from_millis(200);
const CARGO_BIN_EXE_VAR: &str = "CARGO_BIN_EXE_horizon-sessiond";

fn resolve_sessiond_binary() -> PathBuf {
    if let Ok(runtime_var) = std::env::var(CARGO_BIN_EXE_VAR) {
        let path = PathBuf::from(runtime_var);
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from(env!("CARGO_BIN_EXE_horizon-sessiond"))
}

fn spawn_sessiond(command: &mut Command) -> Child {
    match command.spawn() {
        Ok(child) => child,
        Err(first_error) if first_error.kind() == ErrorKind::NotFound => {
            std::thread::sleep(TRANSIENT_LINK_RETRY_DELAY);
            command.spawn().expect("failed to spawn horizon-sessiond")
        }
        Err(error) => panic!("failed to spawn horizon-sessiond: {error}"),
    }
}

struct SessiondProcess {
    child: Child,
    socket_path: PathBuf,
    event_log_path: PathBuf,
    state_db_path: PathBuf,
}

impl SessiondProcess {
    /// Hermetic spawn: throwaway socket/event-log/state-db paths and a
    /// nonexistent config file, exactly like `tests/e2e.rs`'s
    /// `SessiondProcess::spawn` -- never touches a real developer's
    /// `~/.config/horizon` or real event log.
    fn spawn() -> Self {
        let short_id = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let socket_path = std::env::temp_dir().join(format!("hzn-latprobe-{short_id}.sock"));
        let event_log_path =
            std::env::temp_dir().join(format!("hzn-latprobe-events-{short_id}.jsonl"));
        let state_db_path =
            std::env::temp_dir().join(format!("hzn-latprobe-state-{short_id}.duckdb"));
        let missing_config_path =
            std::env::temp_dir().join(format!("hzn-latprobe-no-config-{short_id}.toml"));

        let mut command = Command::new(resolve_sessiond_binary());
        command
            .arg("--socket")
            .arg(&socket_path)
            .env("HORIZON_CONFIG", &missing_config_path)
            .env("HORIZON_AGENT_EVENT_LOG", &event_log_path)
            .env("HORIZON_AGENT_STATE_DB", &state_db_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = spawn_sessiond(&mut command);
        Self {
            child,
            socket_path,
            event_log_path,
            state_db_path,
        }
    }
}

impl Drop for SessiondProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.event_log_path);
        let _ = std::fs::remove_file(&self.state_db_path);
    }
}

async fn connect_with_retry(path: &std::path::Path) -> UnixStream {
    for _ in 0..200 {
        if let Ok(stream) = UnixStream::connect(path).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "horizon-sessiond never accepted a connection on {}",
        path.display()
    );
}

async fn connect_and_handshake(
    socket_path: &std::path::Path,
) -> (BufReader<OwnedReadHalf>, OwnedWriteHalf) {
    let stream = connect_with_retry(socket_path).await;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let hello = session_wire::Envelope::session_control(&SessionControl::Hello(Hello {
        contract_version: horizon_session_protocol::SESSION_PROTOCOL_VERSION,
        binary_id: "latency-probe".to_string(),
    }))
    .unwrap();
    session_wire::write_envelope(&mut write_half, &hello)
        .await
        .unwrap();
    let reply = session_wire::read_envelope(&mut reader)
        .await
        .unwrap()
        .unwrap();
    let control: SessionControl = reply.decode_payload(SESSION_CONTROL_KIND).unwrap();
    assert!(matches!(control, SessionControl::Hello(_)));

    (reader, write_half)
}

async fn write_terminal_control(
    writer: &mut OwnedWriteHalf,
    session_id: uuid::Uuid,
    control: TerminalControl,
) {
    let envelope = encode_terminal_control(Some(session_id), &control).unwrap();
    session_wire::write_envelope(writer, &envelope)
        .await
        .unwrap();
}

async fn write_key(writer: &mut OwnedWriteHalf, session_id: uuid::Uuid, ch: char) {
    let envelope = encode_terminal_command(
        session_id,
        &TerminalCommand::Key {
            key: KeyCode::Char(ch),
            modifiers: Modifiers::NONE,
            event: KeyEventKind::Press,
        },
    )
    .unwrap();
    session_wire::write_envelope(writer, &envelope)
        .await
        .unwrap();
}

/// Reads the next terminal-update envelope for `session_id`, returning the
/// decoded update, the arrival `Instant`, and whether it actually carries
/// grid content different from the frame passed in (`content_changed`) --
/// evidence for the "spurious content-free snapshot" mechanism documented
/// on [`wait_for_marker`].
async fn read_terminal_update_timed(
    reader: &mut BufReader<OwnedReadHalf>,
    session_id: uuid::Uuid,
) -> (TerminalUpdate, Instant) {
    let envelope =
        tokio::time::timeout(Duration::from_secs(5), session_wire::read_envelope(reader))
            .await
            .expect("timed out waiting for a terminal update")
            .unwrap()
            .expect("sessiond should keep the terminal connection open");
    let arrived = Instant::now();
    assert_eq!(envelope.session_id, Some(session_id));
    (decode_terminal_update(&envelope).unwrap(), arrived)
}

fn terminal_spec(shell: &str, args: Vec<String>, cols: u16, rows: u16) -> TerminalSpawnSpec {
    TerminalSpawnSpec {
        shell: shell.to_string(),
        args,
        term: "xterm-256color".into(),
        scrollback_lines: 1_000,
        color_scheme: TerminalColorScheme::default(),
        control_socket: std::env::temp_dir().join("hzn-latprobe-control.sock"),
        fallback_cwd: std::env::temp_dir(),
        spawn_source_session_id: None,
        initial_size: TerminalSize::new(cols, rows),
    }
}

fn percentile(sorted_ms: &[f64], p: f64) -> f64 {
    if sorted_ms.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted_ms.len() - 1) as f64 * p).round() as usize;
    sorted_ms[idx]
}

fn report(label: &str, mut samples_ms: Vec<f64>) {
    samples_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = samples_ms.len();
    let median = percentile(&samples_ms, 0.5);
    let p95 = percentile(&samples_ms, 0.95);
    let min = samples_ms.first().copied().unwrap_or(f64::NAN);
    let max = samples_ms.last().copied().unwrap_or(f64::NAN);
    println!(
        "[latency-probe] {label}: n={n} min={min:.3}ms median={median:.3}ms p95={p95:.3}ms max={max:.3}ms"
    );
}

/// Reads updates, applying each to a running frame reconstruction, until
/// `matches(&frame.text)` is true -- returns the elapsed time from `since`
/// to the update that *actually* satisfied it, how many updates were
/// consumed getting there, and how many of those were "spurious" (arrived
/// but did not change `frame.text` at all).
///
/// Two independent sources of spurious (content-unchanged) updates were
/// found while building this probe, both worth calling out since a naive
/// "wait for the next update" loop mismeasures both as the real echo:
/// - `horizon-terminal-core::session_loop`'s `pty_rx` arm calls
///   `notify_snapshot` unconditionally, even while a BSU window is still
///   open and buffering (no grid mutation went through yet).
/// - Its `key_rx` arm *also* calls `notify_snapshot` unconditionally,
///   right after encoding the keystroke and **before** it has even
///   reached the PTY -- `TerminalCore::key_input` takes `&self` and never
///   mutates `self.term`, so this snapshot is byte-for-byte identical to
///   the previous one. If that spurious send lands within the 16ms
///   coalescing window (`COALESCE_WINDOW`) of the *previous real* update
///   -- i.e. `elapsed >= COALESCE_WINDOW`, which is true for essentially
///   every isolated keystroke on an idle terminal -- it wins the
///   "send immediately" slot and updates `last_sent`, forcing the *real*
///   echo (arriving microseconds to a few ms later, once the PTY
///   actually responds) to fail its own immediate-send check and wait
///   out the full remaining coalescing window instead. See this file's
///   module doc / the investigation report for the numbers this
///   produces.
async fn wait_for_marker(
    reader: &mut BufReader<OwnedReadHalf>,
    session_id: uuid::Uuid,
    frame: &mut horizon_terminal_core::TerminalFrame,
    matches: impl Fn(&str) -> bool,
    since: Instant,
) -> (f64, usize, usize) {
    let mut spurious = 0;
    for reads in 1..=50 {
        let (update, arrived) = read_terminal_update_timed(reader, session_id).await;
        let before = frame.text.clone();
        match update {
            TerminalUpdate::Snapshot(snap) => *frame = snap,
            TerminalUpdate::FrameDiff(diff) => *frame = apply_frame_diff(frame, &diff),
            _ => continue,
        }
        if frame.text == before {
            spurious += 1;
        }
        if matches(&frame.text) {
            return (
                arrived.duration_since(since).as_secs_f64() * 1000.0,
                reads,
                spurious,
            );
        }
    }
    panic!(
        "gave up waiting for the marker; last frame: {:?}",
        frame.text
    );
}

/// Baseline: an interactive `/bin/sh` (dash-family shells rely on the
/// kernel tty line discipline's own canonical-mode echo, not their own
/// readline, so a single keystroke's echo never even wakes the shell
/// process) -- isolates the daemon session-loop + emulation + wire round
/// trip with no child-process compute in the loop at all.
#[tokio::test]
#[ignore = "research probe, not a product gate -- run explicitly, see module doc"]
async fn probe_baseline_shell_echo() {
    let sessiond = SessiondProcess::spawn();
    let session_id = uuid::Uuid::new_v4();
    let (mut reader, mut writer) = connect_and_handshake(&sessiond.socket_path).await;

    write_terminal_control(
        &mut writer,
        session_id,
        TerminalControl::Create(Box::new(terminal_spec(
            "/bin/sh",
            vec!["-i".into()],
            80,
            24,
        ))),
    )
    .await;
    let (TerminalUpdate::Snapshot(mut frame), _) =
        read_terminal_update_timed(&mut reader, session_id).await
    else {
        panic!("expected an initial snapshot");
    };
    // Drain the shell's own startup prompt noise before timing.
    tokio::time::sleep(Duration::from_millis(300)).await;
    loop {
        match tokio::time::timeout(
            Duration::from_millis(50),
            read_terminal_update_timed(&mut reader, session_id),
        )
        .await
        {
            Ok((TerminalUpdate::Snapshot(snap), _)) => frame = snap,
            Ok((TerminalUpdate::FrameDiff(diff), _)) => frame = apply_frame_diff(&frame, &diff),
            Ok(_) => {}
            Err(_) => break,
        }
    }

    let mut round_trip_ms = Vec::new();
    let mut reads_needed = Vec::new();
    let mut spurious_counts = Vec::new();
    // Space keystrokes >16ms apart (see the session loop's COALESCE_WINDOW)
    // so each measurement reflects one echo's own latency, not the
    // coalescing window swallowing back-to-back sends.
    for i in 0..40 {
        let ch = char::from(b'a' + (i % 26) as u8);
        let sent = Instant::now();
        write_key(&mut writer, session_id, ch).await;
        let (ms, reads, spurious) = wait_for_marker(
            &mut reader,
            session_id,
            &mut frame,
            |text| text.trim_end().ends_with(ch),
            sent,
        )
        .await;
        round_trip_ms.push(ms);
        reads_needed.push(reads as f64);
        spurious_counts.push(spurious as f64);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    report(
        "baseline shell echo (round trip, content-verified)",
        round_trip_ms,
    );
    report(
        "baseline shell echo (updates consumed per keystroke)",
        reads_needed,
    );
    report(
        "baseline shell echo (spurious content-free updates per keystroke)",
        spurious_counts,
    );
}

/// Writes `content` to a fresh temp file and returns its path -- used for
/// the synthetic sync-output TUI fixture script.
fn write_fixture(name: &str, content: &str) -> PathBuf {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, content).unwrap();
    path
}

/// A synthetic TUI fixture: on every byte read from stdin, redraws the
/// whole 24-row screen bracketed in DEC private mode 2026 (BSU/ESU),
/// tagging the redraw with an incrementing counter so the probe can find
/// the exact update that carries fresh content. `mode` selects how the
/// window is closed:
/// - `well-behaved`: one `write`+`flush` carrying BSU, content, and ESU.
/// - `multi-chunk`: BSU, then each row as its own `write`+`flush` with a
///   small sleep between a subset of them (some >16ms), then ESU as a
///   final separate write -- probes whether splitting a single redraw
///   across several PTY chunks while the window stays open compounds the
///   coalescing-window cost beyond the single-chunk case.
/// - `delayed-esu`: BSU + content in one write, then a genuine 300ms
///   sleep (simulating slow render), then ESU as a separate write.
/// - `malformed`: BSU + content, and the closing ESU is never sent.
const SYNC_TUI_SCRIPT: &str = r#"
import os, sys, termios, tty, time

mode = sys.argv[1] if len(sys.argv) > 1 else "well-behaved"
fd = sys.stdin.fileno()
tty.setraw(fd)

ROWS = 24

def row_text(r, tag):
    return f"\x1b[{r+1};1H\x1b[2K" + f"row={r} tag={tag} " + ("x" * 40)

def redraw(tag):
    if mode == "multi-chunk":
        sys.stdout.write("\x1b[?2026h")
        sys.stdout.flush()
        sys.stdout.write("\x1b[H")
        sys.stdout.flush()
        for r in range(ROWS):
            sys.stdout.write(row_text(r, tag))
            sys.stdout.flush()
            # A subset of rows sleeps past the 16ms coalescing window;
            # the rest fire back-to-back, mimicking a real renderer's
            # mixed sync/async row writes.
            if r % 6 == 0:
                time.sleep(0.02)
        sys.stdout.write("\x1b[?2026l")
        sys.stdout.flush()
        return

    body = ["\x1b[?2026h", "\x1b[H"]
    for r in range(ROWS):
        body.append(row_text(r, tag))
    if mode == "delayed-esu":
        sys.stdout.write("".join(body))
        sys.stdout.flush()
        time.sleep(0.3)
        sys.stdout.write("\x1b[?2026l")
        sys.stdout.flush()
    elif mode == "malformed":
        sys.stdout.write("".join(body))
        sys.stdout.flush()
        # deliberately never sends the closing ESU
    else:
        body.append("\x1b[?2026l")
        sys.stdout.write("".join(body))
        sys.stdout.flush()

n = 0
while True:
    b = os.read(fd, 1)
    if not b:
        break
    n += 1
    redraw(n)
"#;

async fn run_sync_tui_scenario(label: &str, mode: &str, iterations: usize, gap: Duration) {
    let script = write_fixture(&format!("hzn-latprobe-sync-tui-{mode}.py"), SYNC_TUI_SCRIPT);
    let sessiond = SessiondProcess::spawn();
    let session_id = uuid::Uuid::new_v4();
    let (mut reader, mut writer) = connect_and_handshake(&sessiond.socket_path).await;

    write_terminal_control(
        &mut writer,
        session_id,
        TerminalControl::Create(Box::new(terminal_spec(
            "python3",
            vec!["-u".into(), script.display().to_string(), mode.into()],
            80,
            24,
        ))),
    )
    .await;
    let (TerminalUpdate::Snapshot(mut frame), _) =
        read_terminal_update_timed(&mut reader, session_id).await
    else {
        panic!("expected an initial snapshot");
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut content_ms = Vec::new();
    for i in 1..=iterations {
        let marker = format!("tag={i} ");
        let sent = Instant::now();
        write_key(&mut writer, session_id, 'x').await;
        let (ms, reads, spurious) = wait_for_marker(
            &mut reader,
            session_id,
            &mut frame,
            |t| t.contains(&marker),
            sent,
        )
        .await;
        println!(
            "[latency-probe]   keystroke {i}: {ms:.3}ms after {reads} update(s), {spurious} spurious"
        );
        content_ms.push(ms);
        tokio::time::sleep(gap).await;
    }

    report(label, content_ms);
    let _ = std::fs::remove_file(&script);
}

/// A well-behaved synchronized-update TUI: every keystroke's full-screen
/// redraw is BSU + content + ESU in one `write`+`flush` (one PTY chunk in
/// the overwhelming common case). If the failsafe were somehow firing on
/// well-formed windows too, this would show ~150ms per keystroke instead of
/// near the shell-echo floor.
#[tokio::test]
#[ignore = "research probe, not a product gate -- run explicitly, see module doc"]
async fn probe_synthetic_sync_output_well_behaved() {
    run_sync_tui_scenario(
        "synthetic sync-output TUI (well-behaved BSU..ESU, one write)",
        "well-behaved",
        30,
        Duration::from_millis(30),
    )
    .await;
}

/// The same redraw, but split across many separate `write`+`flush` calls
/// while the sync window stays open (see [`SYNC_TUI_SCRIPT`]'s
/// `multi-chunk` mode) -- probes hypothesis 2 (frame-push cadence beyond
/// the known 16ms window): does a multi-chunk redraw pay the coalescing
/// cost once (at the final ESU) or multiple times along the way?
#[tokio::test]
#[ignore = "research probe, not a product gate -- run explicitly, see module doc"]
async fn probe_synthetic_sync_output_multi_chunk() {
    run_sync_tui_scenario(
        "synthetic sync-output TUI (multi-chunk redraw inside one window)",
        "multi-chunk",
        15,
        Duration::from_millis(300),
    )
    .await;
}

/// A TUI that opens the sync window, writes its content, then genuinely
/// waits 300ms (simulating real render compute, e.g. a slow child) before
/// sending the closing ESU as a *separate* write. Characterizes exactly
/// what the 150ms failsafe (`vte::ansi`'s `SYNC_UPDATE_TIMEOUT`,
/// `crate::core::TerminalCore::sync_flush_deadline`) does to the echo the
/// user sees: does the *content* (not just some update, which
/// `notify_snapshot` can emit even for a still-buffered window -- see
/// `wait_for_marker`) land around the 150ms failsafe, or does it correctly
/// wait for the real ESU at ~300ms because the failsafe only fires once no
/// further PTY data arrives at all (and here, more data *is* coming)?
#[tokio::test]
#[ignore = "research probe, not a product gate -- run explicitly, see module doc"]
async fn probe_synthetic_sync_output_delayed_esu() {
    let script = write_fixture("hzn-latprobe-sync-tui-delayed-esu.py", SYNC_TUI_SCRIPT);
    let sessiond = SessiondProcess::spawn();
    let session_id = uuid::Uuid::new_v4();
    let (mut reader, mut writer) = connect_and_handshake(&sessiond.socket_path).await;

    write_terminal_control(
        &mut writer,
        session_id,
        TerminalControl::Create(Box::new(terminal_spec(
            "python3",
            vec![
                "-u".into(),
                script.display().to_string(),
                "delayed-esu".into(),
            ],
            80,
            24,
        ))),
    )
    .await;
    let (TerminalUpdate::Snapshot(mut frame), _) =
        read_terminal_update_timed(&mut reader, session_id).await
    else {
        panic!("expected an initial snapshot");
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    for i in 1..=3 {
        let marker = format!("tag={i} ");
        let sent = Instant::now();
        write_key(&mut writer, session_id, 'x').await;
        let (ms, reads, spurious) = wait_for_marker(
            &mut reader,
            session_id,
            &mut frame,
            |t| t.contains(&marker),
            sent,
        )
        .await;
        println!(
            "[latency-probe] delayed-esu keystroke {i}: real content landed at {ms:.3}ms \
             (after {reads} update(s), {spurious} spurious)"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = std::fs::remove_file(&script);
}

/// A TUI that opens the sync window and never closes it at all. Confirms
/// (at the real socket/session level, not just the crate-internal unit
/// test `sync_update_failsafe_flushes_a_stuck_window_after_the_deadline` in
/// `horizon-terminal-core::session_loop`) that the failsafe still heals the
/// pane, and measures exactly how long the *real* content (not a spurious
/// empty diff -- see `wait_for_marker`) takes to land end-to-end.
#[tokio::test]
#[ignore = "research probe, not a product gate -- run explicitly, see module doc"]
async fn probe_synthetic_sync_output_malformed() {
    let script = write_fixture("hzn-latprobe-sync-tui-malformed.py", SYNC_TUI_SCRIPT);
    let sessiond = SessiondProcess::spawn();
    let session_id = uuid::Uuid::new_v4();
    let (mut reader, mut writer) = connect_and_handshake(&sessiond.socket_path).await;

    write_terminal_control(
        &mut writer,
        session_id,
        TerminalControl::Create(Box::new(terminal_spec(
            "python3",
            vec![
                "-u".into(),
                script.display().to_string(),
                "malformed".into(),
            ],
            80,
            24,
        ))),
    )
    .await;
    let (TerminalUpdate::Snapshot(mut frame), _) =
        read_terminal_update_timed(&mut reader, session_id).await
    else {
        panic!("expected an initial snapshot");
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    let sent = Instant::now();
    write_key(&mut writer, session_id, 'x').await;
    let (ms, reads, spurious) = wait_for_marker(
        &mut reader,
        session_id,
        &mut frame,
        |t| t.contains("tag=1 "),
        sent,
    )
    .await;
    println!(
        "[latency-probe] malformed (never-closed) sync window: real content healed at {ms:.3}ms \
         (after {reads} update(s), {spurious} spurious)"
    );
    let _ = std::fs::remove_file(&script);
}

/// A fixture app that, immediately on startup (no keypress needed), sends
/// a bare DECRQM query for private mode 2026 (`CSI ?2026$p`) -- with NO
/// synchronized-update window ever opened first -- then reads back
/// whatever bytes the terminal replies with and renders them as visible
/// text (hex-encoded) so the probe can read the literal reply off the
/// wire. This mirrors the *real* Claude Code/ink capability-detection
/// algorithm (decompiled from the installed `claude` binary for this
/// investigation): a single `send` + `flush` of the bare query, raced
/// against a reply, with **no** BSU/ESU bracketing it -- ink treats a
/// DECRPM status of 1 *or* 2 as "supported" and only status 0 (or no
/// reply at all) as "unsupported".
const DECRQM_PROBE_SCRIPT: &str = r#"
import os, sys, termios, tty, time, select, binascii

fd = sys.stdin.fileno()
tty.setraw(fd)

sys.stdout.write("\x1b[?2026$p")
sys.stdout.flush()

reply = b""
deadline = time.time() + 1.0
while time.time() < deadline:
    remaining = deadline - time.time()
    r, _, _ = select.select([fd], [], [], max(0, remaining))
    if not r:
        break
    chunk = os.read(fd, 256)
    if not chunk:
        break
    reply += chunk
    if b"y" in reply:
        # Give a short grace window for any trailing bytes, then stop.
        time.sleep(0.02)
        r2, _, _ = select.select([fd], [], [], 0)
        if r2:
            reply += os.read(fd, 256)
        break

hexed = binascii.hexlify(reply).decode()
sys.stdout.write("\x1b[H\x1b[2K")
sys.stdout.write("DECRQM-REPLY:" + hexed + ":END")
sys.stdout.flush()

while True:
    b = os.read(fd, 1)
    if not b:
        break
"#;

/// Captures the literal DECRQM(2026) reply Horizon's `TerminalCore` sends
/// back for a bare capability probe -- the negotiation trace requested by
/// the investigation: does horizon-terminal-core answer at all, and with
/// what status byte, for the exact query shape a real ink-based app sends
/// before ever opening a synchronized-update window.
#[tokio::test]
#[ignore = "research probe, not a product gate -- run explicitly, see module doc"]
async fn probe_decrqm_negotiation_bare_query() {
    let script = write_fixture("hzn-latprobe-decrqm-probe.py", DECRQM_PROBE_SCRIPT);
    let sessiond = SessiondProcess::spawn();
    let session_id = uuid::Uuid::new_v4();
    let (mut reader, mut writer) = connect_and_handshake(&sessiond.socket_path).await;

    write_terminal_control(
        &mut writer,
        session_id,
        TerminalControl::Create(Box::new(terminal_spec(
            "python3",
            vec!["-u".into(), script.display().to_string()],
            80,
            24,
        ))),
    )
    .await;
    let (TerminalUpdate::Snapshot(mut frame), _) =
        read_terminal_update_timed(&mut reader, session_id).await
    else {
        panic!("expected an initial snapshot");
    };

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut found = None;
    while Instant::now() < deadline {
        match tokio::time::timeout(
            Duration::from_millis(200),
            read_terminal_update_timed(&mut reader, session_id),
        )
        .await
        {
            Ok((TerminalUpdate::Snapshot(snap), _)) => frame = snap,
            Ok((TerminalUpdate::FrameDiff(diff), _)) => frame = apply_frame_diff(&frame, &diff),
            Ok(_) => {}
            Err(_) => {}
        }
        if let Some(start) = frame.text.find("DECRQM-REPLY:") {
            if let Some(end_rel) = frame.text[start..].find(":END") {
                let hex = &frame.text[start + "DECRQM-REPLY:".len()..start + end_rel];
                found = Some(hex.to_string());
                break;
            }
        }
    }

    let hex = found.expect("never observed a DECRQM-REPLY marker in the rendered frame");
    let raw = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect::<Vec<u8>>();
    println!(
        "[latency-probe] DECRQM(2026) bare-query reply: hex={hex} raw={:?} ascii={:?}",
        raw,
        String::from_utf8_lossy(&raw)
    );
    let _ = std::fs::remove_file(&script);
}

/// Best-effort: real Claude Code, hosted by a real `horizon-sessiond`,
/// typed into character by character. Requires a working, already-
/// authenticated `claude` on `PATH` -- skips (with a printed reason)
/// rather than failing if it can't find one, since this is exploratory and
/// must not become a flaky/gated CI-shaped test.
#[tokio::test]
#[ignore = "research probe, not a product gate; needs a real authenticated `claude` -- run explicitly, see module doc"]
async fn probe_real_claude_composer_typing() {
    let Ok(claude_path) = std::process::Command::new("which")
        .arg("claude")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .map_err(|_| ())
        .and_then(|s| if s.is_empty() { Err(()) } else { Ok(s) })
    else {
        println!("[latency-probe] skipping: no `claude` binary found on PATH");
        return;
    };

    let sessiond = SessiondProcess::spawn();
    let session_id = uuid::Uuid::new_v4();
    let (mut reader, mut writer) = connect_and_handshake(&sessiond.socket_path).await;

    write_terminal_control(
        &mut writer,
        session_id,
        TerminalControl::Create(Box::new(terminal_spec(&claude_path, vec![], 120, 40))),
    )
    .await;
    let (TerminalUpdate::Snapshot(mut frame), _) =
        read_terminal_update_timed(&mut reader, session_id).await
    else {
        panic!("expected an initial snapshot");
    };

    // Drain the splash/trust screen for up to 5s, then accept it.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match tokio::time::timeout(
            Duration::from_millis(300),
            read_terminal_update_timed(&mut reader, session_id),
        )
        .await
        {
            Ok((TerminalUpdate::FrameDiff(diff), _)) => frame = apply_frame_diff(&frame, &diff),
            Ok((TerminalUpdate::Snapshot(snap), _)) => frame = snap,
            _ => break,
        }
    }
    // Enter isn't a printable char via KeyCode::Char -- send the literal
    // control byte instead, matching what a real Enter keypress encodes to
    // for a plain (non-kitty) terminal.
    let enter_envelope =
        encode_terminal_command(session_id, &TerminalCommand::Input(b"\r".to_vec())).unwrap();
    session_wire::write_envelope(&mut writer, &enter_envelope)
        .await
        .unwrap();

    // Drain until the composer's ready (best-effort fixed settle time --
    // there's no reliable content marker to wait on across CLI versions).
    let settle_deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < settle_deadline {
        match tokio::time::timeout(
            Duration::from_millis(200),
            read_terminal_update_timed(&mut reader, session_id),
        )
        .await
        {
            Ok((TerminalUpdate::FrameDiff(diff), _)) => frame = apply_frame_diff(&frame, &diff),
            Ok((TerminalUpdate::Snapshot(snap), _)) => frame = snap,
            Ok(_) => {}
            Err(_) => break,
        }
    }

    let mut round_trip_ms = Vec::new();
    let mut reads_needed = Vec::new();
    let mut spurious_counts = Vec::new();
    let mut typed = String::new();
    for ch in "thequickbrownfoxjumps".chars() {
        typed.push(ch);
        let expect_prefix = typed.clone();
        let sent = Instant::now();
        write_key(&mut writer, session_id, ch).await;
        let (ms, reads, spurious) = wait_for_marker(
            &mut reader,
            session_id,
            &mut frame,
            |text| text.contains(&expect_prefix),
            sent,
        )
        .await;
        println!(
            "[latency-probe]   keystroke {ch:?}: {ms:.3}ms after {reads} update(s), {spurious} spurious"
        );
        round_trip_ms.push(ms);
        reads_needed.push(reads as f64);
        spurious_counts.push(spurious as f64);
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    report(
        "real claude composer typing (round trip, content-verified)",
        round_trip_ms,
    );
    report(
        "real claude composer typing (updates consumed per keystroke)",
        reads_needed,
    );
    report(
        "real claude composer typing (spurious content-free updates per keystroke)",
        spurious_counts,
    );
}
