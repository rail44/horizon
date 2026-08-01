//! `horizon-cli`: the CLI client for Horizon's Unix-socket control plane --
//! see `docs/cli-control-plane-design.md` and the frozen contract crate,
//! `crates/horizon-control`. As of the design doc's Second revision
//! ("Single binary, subcommand client"), this crate has no binary of its
//! own -- the root `horizon` binary's `src/main.rs` is the thin argv/env/
//! stdio wrapper that calls [`run`] when it sees a subcommand, dispatching
//! to the GUI (`app_view`) otherwise.
//!
//! Kept as a library so `tests/integration.rs` can drive [`run`] directly
//! against a stub control-plane server (a real
//! `std::os::unix::net::UnixListener`, not the real Horizon) without
//! spawning a subprocess, and so every other piece of logic ([`cli`],
//! [`commands`], [`client`], [`confirm`], [`output`]) is independently
//! unit-testable, colocated with its own module.

mod cli;
mod client;
mod commands;
pub mod confirm;
mod output;

use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;

/// Runs one `horizon-cli` invocation end to end and returns the process
/// exit code:
///
/// - `0`: success.
/// - `1`: connection/handshake/server failure, or a destructive subcommand
///   that was declined (interactively) or lacked `--yes` (non-interactively)
///   -- all of these only become knowable *after* argv was already accepted
///   and (for the destructive case) a live `state` query answered.
/// - `2`: a pure usage error -- [`cli::parse`] rejected argv without ever
///   touching the network, or `send` resolved an empty message body (an
///   empty explicit argument or an empty stdin).
///
/// `stdout`/`stderr`/`stdin` and `ask` are injected so tests can capture output
/// without redirecting the process's real streams and can script the
/// interactive-confirmation answer without a real tty; `stdin` feeds `send`'s
/// stdin-fallback text (the multi-line-brief pasting use case);
/// `stdin_is_tty` is
/// injected for the same reason (`std::io::IsTerminal` on a piped test
/// stdin is always `false`, so a test proving the *interactive* path needs
/// to say so explicitly). `env_socket`/`env_session_id` are the two real
/// `std::env::var` reads (`HORIZON_SOCKET`/`HORIZON_SESSION_ID`), done by
/// the caller so this function stays a pure mapping from its arguments to
/// an exit code plus writes to `stdout`/`stderr`.
#[allow(clippy::too_many_arguments)]
pub fn run(
    args: &[String],
    env_socket: Option<String>,
    env_session_id: Option<String>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    stdin: &mut impl Read,
    stdin_is_tty: bool,
    ask: &mut impl FnMut(&str) -> bool,
) -> u8 {
    let mut parsed = match cli::parse(args) {
        Ok(parsed) => parsed,
        Err(err) => {
            let _ = writeln!(stderr, "{err}");
            return 2;
        }
    };

    // `send`'s text resolution: an explicit positional argument wins;
    // otherwise read stdin to EOF (the multi-line-brief pasting use case).
    // Empty input -- whether an empty explicit argument or an empty stdin
    // -- is a usage error (exit 2), matching the convention `parse` already
    // uses for argv problems. After this block `text` is `Some(non_empty)`.
    if let cli::Subcommand::Send { text, .. } = &mut parsed.subcommand {
        if text.is_none() {
            let mut buf = String::new();
            if let Err(err) = stdin.read_to_string(&mut buf) {
                let _ = writeln!(stderr, "error: failed to read stdin: {err}");
                return 1;
            }
            *text = Some(buf);
        }
        if text.as_ref().is_none_or(|t| t.is_empty()) {
            let _ = writeln!(
                stderr,
                "{}",
                cli::UsageError("send requires non-empty text".to_string())
            );
            return 2;
        }
    }

    let resolved_split = match cli::resolved_split_for(&parsed.subcommand, env_session_id.clone()) {
        Ok(resolved) => resolved,
        Err(message) => {
            let _ = writeln!(stderr, "error: {message}");
            return 1;
        }
    };

    let socket_path = cli::resolve_socket_path(parsed.global.socket.clone(), env_socket);

    let stream = match UnixStream::connect(&socket_path) {
        Ok(stream) => stream,
        Err(err) => {
            let _ = writeln!(
                stderr,
                "error: failed to connect to {}: {err}",
                socket_path.display()
            );
            return 1;
        }
    };
    let writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(err) => {
            let _ = writeln!(stderr, "error: {err}");
            return 1;
        }
    };
    let mut conn = client::Connection::new(BufReader::new(stream), writer);

    if let Err(err) = conn.handshake() {
        let _ = writeln!(stderr, "error: {err}");
        return 1;
    }

    if commands::is_destructive(&parsed.subcommand) {
        let name = commands::external_name(&parsed.subcommand);
        let state = match conn.query_state() {
            Ok(state) => state,
            Err(err) => {
                let _ = writeln!(stderr, "error: {err}");
                return 1;
            }
        };
        if state.destructive_commands.iter().any(|c| c == name) {
            if let Err(message) = confirm::resolve(parsed.global.yes, stdin_is_tty, name, ask) {
                let _ = writeln!(stderr, "error: {message}");
                return 1;
            }
        }
    }

    // The issuer session id (from `HORIZON_SESSION_ID`) is carried as the
    // `"issuer"` wire key so the shell can parent the new session to the
    // dispatching pane rather than the focused pane (issue 013). An empty
    // string is treated as absent -- it is not a valid session id.
    let issuer = env_session_id.as_deref().filter(|value| !value.is_empty());
    let request = commands::to_request(&parsed.subcommand, resolved_split.as_deref(), issuer);
    match conn.send_request(request) {
        Ok(body) => {
            output::render(&body, parsed.global.json, stdout);
            0
        }
        Err(err) => {
            let _ = writeln!(stderr, "error: {err}");
            1
        }
    }
}
