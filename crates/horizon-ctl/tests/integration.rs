//! Integration tests against a stub control-plane server: a real
//! `std::os::unix::net::UnixListener` speaking `horizon_control::wire`
//! directly, standing in for Horizon's control endpoint (per the task's
//! instruction: "本物の Horizon には繋がない"). Drives `horizon_ctl::run`
//! (the same entry point `main.rs` calls) end to end -- socket resolution,
//! handshake, one invoke/query round trip, `Rejected` handling, the id
//! echo, and the destructive `--yes` path -- exactly as a real invocation
//! would, minus the process boundary.

use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;

use horizon_control::contract::{
    Envelope, EnvelopeBody, ErrorMessage, HelloAck, Rejected, SessionEntry, Sessions, State,
    CONTROL_VERSION,
};
use horizon_control::wire;

/// One accepted connection's worth of stub-server behavior: read `hello`,
/// reply per `hello_reply`, then read/reply pairs from `exchanges` in
/// order. Runs on its own thread so the test's own call into
/// `horizon_ctl::run` can block on the connection like a real client would.
fn stub_server(socket_path: PathBuf, hello_reply: EnvelopeBody, exchanges: Vec<EnvelopeBody>) {
    let listener = UnixListener::bind(&socket_path).expect("bind stub socket");
    thread::spawn(move || {
        let (stream, _addr) = listener.accept().expect("accept one connection");
        serve_one_connection(stream, hello_reply, exchanges);
    });
}

fn serve_one_connection(
    stream: UnixStream,
    hello_reply: EnvelopeBody,
    exchanges: Vec<EnvelopeBody>,
) {
    let mut writer = stream.try_clone().expect("clone stream for writing");
    let mut reader = BufReader::new(stream);

    let hello = wire::read_envelope(&mut reader)
        .expect("read hello")
        .expect("connection open for hello");
    wire::write_envelope(&mut writer, &Envelope::new(hello.id, hello_reply)).unwrap();

    for reply_body in exchanges {
        let Some(request) = wire::read_envelope(&mut reader).expect("read request") else {
            break;
        };
        wire::write_envelope(&mut writer, &Envelope::new(request.id, reply_body)).unwrap();
    }
}

fn temp_socket_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "horizon-ctl-it-{label}-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn our_hello_ack() -> EnvelopeBody {
    EnvelopeBody::HelloAck(HelloAck {
        control_version: CONTROL_VERSION,
        binary_id: "horizon-stub 0.0.0".to_string(),
        capabilities: vec!["sessions".to_string(), "state".to_string()],
    })
}

/// Runs `horizon_ctl::run` with a fixed, never-asked confirmation callback
/// (tests that need the interactive path pass their own `ask` inline
/// instead) and returns `(exit_code, stdout, stderr)`.
fn run_ctl(args: &[&str], socket_path: &Path, stdin_is_tty: bool) -> (u8, String, String) {
    run_ctl_with_ask(args, socket_path, stdin_is_tty, &mut |_| {
        panic!("ask() should not be called in this test")
    })
}

fn run_ctl_with_ask(
    args: &[&str],
    socket_path: &Path,
    stdin_is_tty: bool,
    ask: &mut impl FnMut(&str) -> bool,
) -> (u8, String, String) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = horizon_ctl::run(
        &args,
        Some(socket_path.display().to_string()),
        None,
        &mut stdout,
        &mut stderr,
        stdin_is_tty,
        ask,
    );
    (
        code,
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

#[test]
fn sessions_query_round_trips_through_the_handshake() {
    let socket_path = temp_socket_path("sessions");
    stub_server(
        socket_path.clone(),
        our_hello_ack(),
        vec![EnvelopeBody::Sessions(Sessions {
            sessions: vec![SessionEntry {
                session_id: "s-1".to_string(),
                kind: "agent".to_string(),
                attached: true,
                title: "agent: fix bug".to_string(),
            }],
        })],
    );

    let (code, stdout, stderr) = run_ctl(&["sessions"], &socket_path, false);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "s-1  agent  attached  agent: fix bug\n");

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn state_query_supports_json_output() {
    let socket_path = temp_socket_path("state-json");
    stub_server(
        socket_path.clone(),
        our_hello_ack(),
        vec![EnvelopeBody::State(State {
            tab_count: 2,
            visible_pane_count: 3,
            has_active_session: true,
            detached_session_count: 1,
            has_pending_approval: false,
            has_turn_in_flight: true,
            destructive_commands: vec!["terminate-session".to_string()],
        })],
    );

    let (code, stdout, stderr) = run_ctl(&["--json", "state"], &socket_path, false);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("\"kind\": \"state\""));
    assert!(stdout.contains("\"tab_count\": 2"));

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn invoke_new_terminal_reports_ok() {
    let socket_path = temp_socket_path("new-terminal");
    stub_server(socket_path.clone(), our_hello_ack(), vec![EnvelopeBody::Ok]);

    let (code, stdout, stderr) = run_ctl(&["new-terminal"], &socket_path, false);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "OK\n");

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn new_agent_with_prompt_reaches_the_server_and_reports_ok() {
    let socket_path = temp_socket_path("new-agent-prompt");
    let listener = UnixListener::bind(&socket_path).expect("bind stub socket");
    let received_command: std::sync::Arc<std::sync::Mutex<Option<(String, serde_json::Value)>>> =
        std::sync::Arc::default();
    let received_command_clone = received_command.clone();
    thread::spawn(move || {
        let (stream, _addr) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);

        let hello = wire::read_envelope(&mut reader).unwrap().unwrap();
        wire::write_envelope(&mut writer, &Envelope::new(hello.id, our_hello_ack())).unwrap();

        let request = wire::read_envelope(&mut reader).unwrap().unwrap();
        if let EnvelopeBody::Invoke(invoke) = request.body {
            *received_command_clone.lock().unwrap() = Some((invoke.command, invoke.args));
        }
        wire::write_envelope(&mut writer, &Envelope::new(request.id, EnvelopeBody::Ok)).unwrap();
    });

    let (code, stdout, stderr) = run_ctl(
        &["new-agent", "--prompt", "fix the bug"],
        &socket_path,
        false,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "OK\n");

    let received = received_command
        .lock()
        .unwrap()
        .clone()
        .expect("server saw a request");
    assert_eq!(received.0, "new-agent");
    assert_eq!(
        received.1,
        serde_json::json!({ "activate": false, "prompt": "fix the bug" })
    );

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn rejected_handshake_is_reported_and_exits_with_a_server_error_code() {
    let socket_path = temp_socket_path("rejected");
    stub_server(
        socket_path.clone(),
        EnvelopeBody::Rejected(Rejected {
            reason: "control version mismatch".to_string(),
        }),
        vec![],
    );

    let (code, stdout, stderr) = run_ctl(&["sessions"], &socket_path, false);
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("control version mismatch"),
        "stderr: {stderr}"
    );

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn server_error_reply_is_reported_and_exits_with_a_server_error_code() {
    let socket_path = temp_socket_path("server-error");
    stub_server(
        socket_path.clone(),
        our_hello_ack(),
        vec![EnvelopeBody::Error(ErrorMessage {
            message: "no such session".to_string(),
        })],
    );

    let (code, _stdout, stderr) = run_ctl(&["cancel-turn", "s-1"], &socket_path, false);
    assert_eq!(code, 1);
    assert!(stderr.contains("no such session"), "stderr: {stderr}");

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn response_id_mismatch_is_detected_as_a_protocol_violation() {
    let socket_path = temp_socket_path("id-mismatch");
    let listener = UnixListener::bind(&socket_path).expect("bind stub socket");
    thread::spawn(move || {
        let (stream, _addr) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);

        let hello = wire::read_envelope(&mut reader).unwrap().unwrap();
        // Deliberately reply with the wrong id to prove the client catches
        // it instead of blindly trusting response ordering.
        wire::write_envelope(&mut writer, &Envelope::new(hello.id + 41, our_hello_ack())).unwrap();
    });

    let (code, _stdout, stderr) = run_ctl(&["sessions"], &socket_path, false);
    assert_eq!(code, 1);
    assert!(stderr.contains("id mismatch"), "stderr: {stderr}");

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn destructive_subcommand_without_yes_is_refused_when_not_a_tty() {
    let socket_path = temp_socket_path("destructive-no-yes");
    stub_server(
        socket_path.clone(),
        our_hello_ack(),
        vec![EnvelopeBody::State(State {
            tab_count: 0,
            visible_pane_count: 0,
            has_active_session: false,
            detached_session_count: 1,
            has_pending_approval: false,
            has_turn_in_flight: false,
            destructive_commands: vec!["terminate-all-detached".to_string()],
        })],
    );

    let (code, stdout, stderr) = run_ctl(&["terminate-all-detached"], &socket_path, false);
    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    assert!(stderr.contains("--yes"), "stderr: {stderr}");

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn destructive_subcommand_with_yes_proceeds_without_a_tty() {
    let socket_path = temp_socket_path("destructive-yes");
    stub_server(
        socket_path.clone(),
        our_hello_ack(),
        vec![
            EnvelopeBody::State(State {
                tab_count: 0,
                visible_pane_count: 0,
                has_active_session: false,
                detached_session_count: 1,
                has_pending_approval: false,
                has_turn_in_flight: false,
                destructive_commands: vec!["terminate-all-detached".to_string()],
            }),
            EnvelopeBody::Ok,
        ],
    );

    let (code, stdout, stderr) = run_ctl(&["terminate-all-detached", "--yes"], &socket_path, false);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "OK\n");

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn destructive_subcommand_not_listed_by_the_server_skips_confirmation() {
    // The server's `destructive_commands` doesn't (yet) list
    // `terminate-session` -- e.g. an older server build -- so `horizon-ctl`
    // should not block a non-tty caller lacking `--yes`.
    let socket_path = temp_socket_path("destructive-not-listed");
    stub_server(
        socket_path.clone(),
        our_hello_ack(),
        vec![
            EnvelopeBody::State(State {
                tab_count: 0,
                visible_pane_count: 0,
                has_active_session: false,
                detached_session_count: 0,
                has_pending_approval: false,
                has_turn_in_flight: false,
                destructive_commands: vec![],
            }),
            EnvelopeBody::Ok,
        ],
    );

    let (code, stdout, stderr) = run_ctl(&["terminate-session", "s-1"], &socket_path, false);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "OK\n");

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn destructive_subcommand_on_a_tty_asks_and_honors_the_answer() {
    let socket_path = temp_socket_path("destructive-tty");
    stub_server(
        socket_path.clone(),
        our_hello_ack(),
        vec![
            EnvelopeBody::State(State {
                tab_count: 0,
                visible_pane_count: 0,
                has_active_session: false,
                detached_session_count: 1,
                has_pending_approval: false,
                has_turn_in_flight: false,
                destructive_commands: vec!["terminate-all-detached".to_string()],
            }),
            EnvelopeBody::Ok,
        ],
    );

    let mut asked_for = None;
    let (code, stdout, stderr) = run_ctl_with_ask(
        &["terminate-all-detached"],
        &socket_path,
        true,
        &mut |name| {
            asked_for = Some(name.to_string());
            true
        },
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "OK\n");
    assert_eq!(asked_for.as_deref(), Some("terminate-all-detached"));

    let _ = std::fs::remove_file(&socket_path);
}

#[test]
fn a_socket_with_no_listener_is_a_clear_connection_error_not_a_panic() {
    // Deliberately an explicit `--socket` to a path nothing is bound to,
    // rather than leaving both `--socket` and `HORIZON_SOCKET` unset: since
    // the Second revision's fixed default path is real and per-user (see
    // `cli::resolve_socket_path`), leaving both unset in a test would race
    // whatever real Horizon instance happens to be running on this machine
    // instead of deterministically exercising the "nothing is listening"
    // path this test means to cover.
    let socket_path = temp_socket_path("no-listener");
    let args: Vec<String> = vec![
        "--socket".to_string(),
        socket_path.display().to_string(),
        "sessions".to_string(),
    ];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = horizon_ctl::run(
        &args,
        None,
        None,
        &mut stdout,
        &mut stderr,
        false,
        &mut |_| false,
    );
    assert_eq!(code, 1);
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .contains("failed to connect"),
        "expected a clear connection-failure message"
    );
}

#[test]
fn usage_error_exits_with_code_two() {
    let args: Vec<String> = vec!["not-a-real-subcommand".to_string()];
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = horizon_ctl::run(
        &args,
        Some("/tmp/should-not-be-used.sock".to_string()),
        None,
        &mut stdout,
        &mut stderr,
        false,
        &mut |_| panic!("must not get far enough to ask"),
    );
    assert_eq!(code, 2);
}
