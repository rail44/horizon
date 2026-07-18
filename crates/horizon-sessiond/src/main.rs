//! `horizon-sessiond`: steps 2-4 of `docs/agent-runtime-split-design.md`'s
//! agent-runtime split. Owns the Unix socket, the `hello` handshake, and (as
//! of step 3) real agent sessions: `session_new` spawns the provider/tool/
//! persistence machinery this binary hosts (see `session::run_session`),
//! command/event envelopes route by session id, and this process owns the
//! event log + DuckDB projection -- Horizon never opens either itself. As of
//! step 4, every session found in the log at startup is resumed live (see
//! `session::resume_persisted_sessions`) and `session_load` re-emits a
//! session's committed events to a (re)connecting client.
//!
//! **Bind first (startup ordering).** [`main`] binds/listens on the socket
//! as its first action after arg/config parsing -- before it ever reads the
//! event log or resumes a single session. A large event log's read plus
//! resuming many sessions can take real wall-clock time (seconds, not
//! milliseconds), and the old ordering did all of that *before* binding: a
//! client racing to connect during that window would time out its own
//! retry budget, conclude nothing was listening, and spawn a second
//! `horizon-sessiond` -- which itself would replay the whole log a second
//! time before discovering the first instance already owns the socket (see
//! `bind_listener`'s stale-socket handling). Binding first makes that whole
//! failure mode structurally impossible in the normal path: a client's
//! `connect` succeeds (queued by the kernel) the instant `listen` returns,
//! long before any persistence work starts, and a genuine second instance
//! now hits `bind_listener`'s "already accepting" bail immediately, before
//! it ever opens its own log.
//!
//! The event-log read and session resume move to a background task
//! ([`spawn_resume_task`]) that races the accept loop. `hello`/`ping` never
//! touch session state, so they're answered immediately regardless of
//! whether that background work has finished; `session_list`/`session_load`/
//! `session_new` would return an incomplete (or, right after bind, empty)
//! view of history if answered too early, so they block on [`session::
//! SessiondState::wait_until_resume_ready`] first -- a readiness gate, not a
//! protocol change.
//!
//! **The DuckDB rebuild is off the readiness path too.** It used to run
//! synchronously inside [`open_persistence`], *before* [`SessiondState::
//! set_writer`]/[`session::resume_persisted_sessions`]/[`SessiondState::
//! mark_resume_ready`] -- meaning every readiness-gated request waited on a
//! full synchronous rebuild of a derived, non-authoritative read model that
//! no session actually needs (sessions resume from the JSONL log directly).
//!
//! As of the recall work, the rebuild (and the projection itself) moved
//! again: it's no longer a separate task this binary spawns at all.
//! [`open_persistence`] now passes `agent_config.persistence.duckdb_path`
//! straight into [`WriterHandle::open_silently`], and the event log's own
//! background writer thread (`horizon_agent::persistence::event_log::
//! writer`) does the rebuild-or-skip itself, right after it sends
//! `WriterInit::Ready` (which is what [`open_persistence`]'s blocking
//! `init_rx.recv()` -- and therefore [`SessiondState::mark_resume_ready`] --
//! actually waits on) and before it starts draining/durably writing further
//! appends. Readiness still doesn't wait on DuckDB at all, and the same
//! skip-when-current freshness check still applies (see that module's
//! `rebuild_and_open_duckdb_projection` doc comment) -- but the writer
//! thread now *keeps* the opened `Store` afterward instead of dropping it,
//! projecting every later append live instead of only at the next restart
//! (`docs/agent-duckdb-state-design.md`'s "Runtime Boundary" addendum).

mod session;
mod terminal;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use horizon_agent::config::AgentConfig;
use horizon_agent::contract::ProviderRegistry;
use horizon_agent::persistence::event_log::{Record, WriterHandle, WriterInit};
use horizon_agent::persistence::projection::duckdb::{DuckdbStoreHandle, SharedDuckdbStore};
use horizon_agent::socket::default_socket_path;
use horizon_agent::wire::{self as agent_wire, Control, Envelope, EnvelopeBody, CONTRACT_VERSION};
use horizon_session_protocol::{
    self as session_wire, Envelope as RawEnvelope, Hello, SessionControl, SESSION_CONTROL_KIND,
};
use horizon_terminal_core::{
    decode_terminal_command, decode_terminal_control, TerminalControl, TERMINAL_COMMAND_KIND,
    TERMINAL_CONTROL_KIND,
};
use session::{Connection, SessiondState};
use terminal::TerminalHost;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{
    unix::{OwnedReadHalf, OwnedWriteHalf},
    UnixListener, UnixStream,
};

/// Reported in this binary's `hello` reply's `binary_id`. The semantic
/// contract version is carried separately in the same [`Hello`].
const BINARY_ID: &str = concat!("horizon-sessiond/", env!("CARGO_PKG_VERSION"));

/// Test-only hook (`crates/horizon-sessiond/tests/e2e.rs`): when set to a
/// number of milliseconds, [`spawn_resume_task`] sleeps that long before
/// opening the event log, so a test can prove the bind-first ordering
/// (hello answers well before this delay elapses; `session_list`/
/// `session_load` don't) instead of relying on incidental timing. Never set
/// in production.
const TEST_RESUME_DELAY_MS_VAR: &str = "HORIZON_SESSIOND_TEST_RESUME_DELAY_MS";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let socket_path =
        socket_path_from_args(std::env::args().skip(1)).unwrap_or_else(default_socket_path);

    // `horizon-sessiond` is now the one process that reads Horizon's config
    // file directly (see `docs/agent-runtime-split-design.md`'s "the child
    // owns the event log and DuckDB projection", extended by the
    // 2026-07-18 config-narrowing wave's consolidation onto
    // `horizon-config`): only `[provider]` `model`/`base_url` still vary
    // per this crate's config -- everything else former `[agent]` knobs
    // became fixed built-in constants in `horizon_agent::config` (see that
    // module's doc).
    let raw_config = horizon_config::load();
    let agent_config = AgentConfig::from_env_and_provider(
        raw_config.provider.model.clone(),
        raw_config.provider.base_url.clone(),
    );
    // Resolved once at startup and handed to every session's
    // `ToolSessionState` (see `SessiondState::config_path`/`run_session`):
    // the `config.read`/`config.write` agent tools' one and only target.
    let config_path = horizon_config::resolved_path();
    eprintln!(
        "horizon-sessiond: starting on {} (model={})",
        socket_path.display(),
        agent_config.rig.model
    );

    // Bind-first (see the module doc): this is the first thing that touches
    // the socket path, and it happens before the event log is even opened.
    let listener = bind_listener(&socket_path).await?;

    // Shared, multi-reader-blocking handle onto the live DuckDB projection
    // -- see `SharedDuckdbStore`'s doc comment. Created empty here (before
    // the event log's writer thread, and therefore any real DuckDB store,
    // exists) and handed to both the provider registry (the rig provider's
    // history replay) and `SessiondState` (the recall tools' context);
    // populated later, once, by `spawn_resume_task`'s propagator.
    let duckdb_cell = SharedDuckdbStore::new();
    let providers =
        ProviderRegistry::builtin_with_config(agent_config.clone(), duckdb_cell.clone());
    let state = Arc::new(SessiondState::new(
        providers,
        agent_config.clone(),
        None,
        duckdb_cell.clone(),
        config_path,
    ));

    spawn_resume_task(state.clone(), agent_config, duckdb_cell);
    let terminals = TerminalHost::new();

    run(listener, &socket_path, state, terminals).await
}

/// Opens this process's own event log writer, resumes every session found
/// in it, and marks `state` ready -- all on a background task that races
/// the accept loop `main` starts right after this returns, per the
/// module's bind-first fix. Decision 3 in the design doc ("the child owns
/// the event log and DuckDB projection") still holds: this uses this
/// binary's own config load, never Horizon's.
///
/// The DuckDB rebuild is no longer spawned from here at all -- see the
/// module doc's "DuckDB rebuild is off the readiness path too" for where it
/// actually happens now ([`open_persistence`]'s `WriterHandle::
/// open_silently` call).
///
/// `duckdb_cell` is populated by a *separate*, fire-and-forget
/// `spawn_blocking` task spawned right after `open_persistence` returns --
/// deliberately not awaited by (or ordered against) `resume_persisted_
/// sessions`/`mark_resume_ready` below, so populating it can never
/// reintroduce DuckDB onto the readiness path this function's own doc
/// comment describes avoiding.
fn spawn_resume_task(
    state: Arc<SessiondState>,
    agent_config: AgentConfig,
    duckdb_cell: SharedDuckdbStore,
) {
    tokio::task::spawn_blocking(move || {
        if let Some(delay) = test_resume_delay() {
            std::thread::sleep(delay);
        }
        let (writer, records, skipped_lines_summary, duckdb_ready_rx) =
            open_persistence(&agent_config);
        state.set_writer(writer);
        state.set_skipped_lines_summary(skipped_lines_summary);
        // Step 4: "sessiond restart = read own log, rebuild rig_history, mark
        // turns that died mid-flight as cancelled ... sessions are live
        // again".
        session::resume_persisted_sessions(&state, records);
        state.mark_resume_ready();

        // Propagates the writer thread's own rebuild-or-open decision into
        // the shared cell whenever it lands -- `duckdb_ready_rx.recv()`
        // blocks this dedicated propagator task, never the resume/readiness
        // path above (already completed by the time this line runs). A
        // disconnected channel (the writer's startup itself failed, so it
        // never reached the DuckDB step at all) is treated the same as an
        // explicit "no store".
        tokio::task::spawn_blocking(move || {
            let store = duckdb_ready_rx.recv().ok().flatten();
            duckdb_cell.set(store);
        });
    });
}

fn test_resume_delay() -> Option<Duration> {
    std::env::var(TEST_RESUME_DELAY_MS_VAR)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
}

/// Opens this process's own event log writer, using this binary's own
/// config load (never Horizon's). Blocks the calling (background, per
/// [`spawn_resume_task`]) task on the writer's startup read -- simpler than
/// threading the `WriterInit` channel further, and no longer something the
/// accept loop waits on (see the module doc). Also hands back the startup
/// read's records (empty if persistence is disabled or there's nothing yet)
/// so the caller can resume every session they belong to
/// ([`session::resume_persisted_sessions`]), and the human-readable
/// skipped-lines summary (if any corrupt/torn lines were found) so
/// [`spawn_resume_task`] can stash it on [`SessiondState`] for `main::
/// run_session_hosting_loop` to forward to a connecting client -- restoring
/// the step-3 trim recorded in `docs/agent-runtime-split-design.md`
/// ("Skipped-lines status reporting is omitted").
///
/// Opens via [`WriterHandle::open_silently`] rather than [`WriterHandle::
/// open`]: this function already prints its own `horizon-sessiond`-prefixed
/// skipped-lines summary just below, so the shared writer module's own
/// generic summary line would otherwise double up in sessiond's stderr.
/// `open_silently`'s `duckdb_path` argument is this binary's only real
/// production use of the live DuckDB projection (see the module doc): the
/// event log's own writer thread rebuilds (or skips, if already current)
/// and then keeps that `Store` open for the rest of this process's life.
///
/// The fourth return value is `open_silently`'s own second receiver: the
/// writer thread's DuckDB rebuild-or-open decision, delivered exactly once,
/// whenever it lands (see [`spawn_resume_task`]'s propagator, which is the
/// only consumer). A disconnected channel (writer startup itself failed)
/// is indistinguishable from -- and handled the same as -- an explicit
/// `None`.
fn open_persistence(
    agent_config: &AgentConfig,
) -> (
    Option<WriterHandle>,
    Vec<Record>,
    Option<String>,
    crossbeam_channel::Receiver<Option<DuckdbStoreHandle>>,
) {
    let (writer, init_rx, duckdb_rx) = WriterHandle::open_silently(
        &agent_config.persistence.event_log_path,
        agent_config.persistence.duckdb_path.clone(),
    );
    match init_rx.recv() {
        Ok(WriterInit::Ready(report)) => {
            let skipped_summary = report.skipped_summary();
            if let Some(summary) = &skipped_summary {
                eprintln!(
                    "horizon-sessiond: {summary} while opening {}",
                    agent_config.persistence.event_log_path.display()
                );
            }
            (Some(writer), report.records, skipped_summary, duckdb_rx)
        }
        Ok(WriterInit::Failed(error)) => {
            eprintln!(
                "horizon-sessiond: event log unavailable ({error}); persistence disabled for this run"
            );
            (None, Vec::new(), None, duckdb_rx)
        }
        Err(_) => {
            eprintln!(
                "horizon-sessiond: event log writer thread exited before reporting startup status; \
                 persistence disabled for this run"
            );
            (None, Vec::new(), None, duckdb_rx)
        }
    }
}

async fn run(
    listener: UnixListener,
    socket_path: &Path,
    state: Arc<SessiondState>,
    terminals: TerminalHost,
) -> anyhow::Result<()> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _addr) = accepted?;
                if let Err(err) = handle_connection(stream, state.clone(), terminals.clone()).await {
                    eprintln!("horizon-sessiond: connection error: {err}");
                }
            }
            _ = sigterm.recv() => {
                eprintln!("horizon-sessiond: SIGTERM received, shutting down");
                terminals.shutdown_all();
                break;
            }
        }
    }

    let _ = std::fs::remove_file(socket_path);
    Ok(())
}

/// One connection at a time by construction: [`run`]'s accept loop awaits
/// this to completion before accepting the next connection (multi-client
/// support is explicitly out of scope -- see the design doc's "Out of scope
/// here"). Two phases: a fully sequential handshake (identical to step 2 --
/// must succeed before any concurrency starts, so a rejected/mismatched
/// hello can reply and close deterministically without racing an
/// independent writer task), then [`run_session_hosting_loop`], which needs
/// genuine read/write concurrency since a hosted session can push events at
/// any time, not just in reply to something Horizon sent.
async fn handle_connection(
    stream: UnixStream,
    state: Arc<SessiondState>,
    terminals: TerminalHost,
) -> anyhow::Result<()> {
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    loop {
        let envelope = match session_wire::read_envelope(&mut reader).await {
            Ok(Some(envelope)) => envelope,
            Ok(None) => return Ok(()),
            Err(err) => {
                eprintln!("horizon-sessiond: malformed message, closing connection: {err}");
                return Ok(());
            }
        };

        if envelope.kind != SESSION_CONTROL_KIND {
            eprintln!("horizon-sessiond: domain message received before hello, ignoring");
            continue;
        }
        let control: SessionControl = match envelope.decode_payload(SESSION_CONTROL_KIND) {
            Ok(control) => control,
            Err(error) => {
                eprintln!("horizon-sessiond: malformed shared control before hello: {error}");
                return Ok(());
            }
        };

        match control {
            SessionControl::Hello(hello) => {
                if hello.contract_version != CONTRACT_VERSION {
                    let reason = format!(
                        "contract version mismatch: horizon-sessiond speaks v{CONTRACT_VERSION}, \
                         client sent v{} -- reload required",
                        hello.contract_version
                    );
                    eprintln!("horizon-sessiond: rejecting handshake: {reason}");
                    let rejected =
                        RawEnvelope::session_control(&SessionControl::HandshakeRejected(reason))?;
                    let _ = session_wire::write_envelope(&mut writer, &rejected).await;
                    return Ok(());
                }
                session_wire::write_envelope(&mut writer, &our_hello_envelope()?).await?;
                break;
            }
            SessionControl::Ping => {
                let pong = RawEnvelope::session_control(&SessionControl::Pong)?;
                session_wire::write_envelope(&mut writer, &pong).await?;
            }
            SessionControl::Drain => {
                let _ = writer.flush().await;
                terminals.shutdown_all();
                flush_event_log_before_exit(state.writer());
                eprintln!("horizon-sessiond: drained, exiting");
                std::process::exit(0);
            }
            other => eprintln!("horizon-sessiond: {other:?} received before hello, ignoring"),
        }
    }

    run_session_hosting_loop(reader, writer, state, terminals).await
}

/// The post-handshake phase: a writer task owns the socket's write half and
/// drains an outgoing-envelope queue (fed by both this loop's own replies
/// and every hosted session's thread -- see [`Connection`]), while this
/// function keeps reading incoming envelopes and routing them. Splitting
/// read and write this way is what lets a session push events (or a
/// `host_tool_request`) to Horizon asynchronously, not just in response to
/// something Horizon just sent.
///
/// The writer task is deliberately never awaited to completion here: on a
/// normal disconnect, session threads spawned by [`Connection`] may still
/// hold `outgoing` senders (sessions outlive the connection that created
/// them within this process -- see `session`'s module doc), so the channel
/// would never close and awaiting the writer task would hang this function
/// forever, wedging the accept loop against ever serving a next connection.
/// Letting it run detached means it simply keeps trying to write to a dead
/// socket until its next send fails, at which point it exits on its own.
async fn run_session_hosting_loop(
    mut reader: BufReader<OwnedReadHalf>,
    mut writer: OwnedWriteHalf,
    state: Arc<SessiondState>,
    terminals: TerminalHost,
) -> anyhow::Result<()> {
    let (raw_outgoing_tx, mut raw_outgoing_rx) =
        tokio::sync::mpsc::unbounded_channel::<RawEnvelope>();
    let (agent_outgoing_tx, mut agent_outgoing_rx) =
        tokio::sync::mpsc::unbounded_channel::<Envelope>();
    let connection = Connection::new(agent_outgoing_tx.clone(), state);
    terminals.connect(raw_outgoing_tx.clone());

    let raw_agent_tx = raw_outgoing_tx.clone();
    tokio::spawn(async move {
        while let Some(envelope) = agent_outgoing_rx.recv().await {
            if let Ok(raw) = agent_wire::encode_envelope(&envelope) {
                if raw_agent_tx.send(raw).is_err() {
                    break;
                }
            }
        }
    });

    tokio::spawn(async move {
        while let Some(envelope) = raw_outgoing_rx.recv().await {
            if session_wire::write_envelope(&mut writer, &envelope)
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // Step-3 trim, restored: report this process's own startup event-log
    // corruption diagnostics once for this connection, without blocking
    // this function's own read loop (or `hello`, answered before this
    // function ever runs) on the same readiness gate `SessionList`/
    // `SessionLoad` already use -- see `SessiondState::skipped_lines_summary`'s
    // doc comment and `docs/agent-runtime-split-design.md`'s step 3 notes.
    {
        let connection = connection.clone();
        let outgoing_tx = agent_outgoing_tx.clone();
        tokio::spawn(async move {
            connection.wait_until_resume_ready().await;
            if let Some(summary) = connection.skipped_lines_summary() {
                let _ = outgoing_tx.send(Envelope::control(Control::SkippedLines(summary)));
            }
        });
    }

    loop {
        let raw = match session_wire::read_envelope(&mut reader).await {
            Ok(Some(envelope)) => envelope,
            Ok(None) => {
                connection.disconnect();
                terminals.disconnect();
                return Ok(());
            }
            Err(err) => {
                eprintln!("horizon-sessiond: malformed message, closing connection: {err}");
                connection.disconnect();
                terminals.disconnect();
                return Ok(());
            }
        };

        if raw.kind == SESSION_CONTROL_KIND {
            match raw.decode_payload::<SessionControl>(SESSION_CONTROL_KIND) {
                Ok(SessionControl::Ping) => {
                    if let Ok(pong) = RawEnvelope::session_control(&SessionControl::Pong) {
                        let _ = raw_outgoing_tx.send(pong);
                    }
                }
                Ok(SessionControl::Drain) => {
                    terminals.shutdown_all();
                    flush_event_log_before_exit(connection.writer());
                    eprintln!("horizon-sessiond: drained, exiting");
                    std::process::exit(0);
                }
                Ok(other) => {
                    eprintln!("horizon-sessiond: unexpected shared control: {other:?}");
                }
                Err(error) => eprintln!("horizon-sessiond: malformed shared control: {error}"),
            }
            continue;
        }

        if raw.kind == TERMINAL_CONTROL_KIND {
            match decode_terminal_control(&raw) {
                Ok(control @ TerminalControl::List { .. }) if raw.session_id.is_none() => {
                    terminals.handle_control(None, control);
                }
                Ok(control @ (TerminalControl::Create(_) | TerminalControl::Attach { .. }))
                    if raw.session_id.is_some() =>
                {
                    terminals.handle_control(raw.session_id, control);
                }
                Ok(control) => {
                    eprintln!(
                        "horizon-sessiond: terminal control has invalid scope or direction: \
                         {control:?}"
                    );
                }
                Err(error) => eprintln!("horizon-sessiond: malformed terminal control: {error}"),
            }
            continue;
        }

        if raw.kind == TERMINAL_COMMAND_KIND {
            let Some(session_id) = raw.session_id else {
                eprintln!("horizon-sessiond: terminal command missing session_id, ignoring");
                continue;
            };
            match decode_terminal_command(&raw) {
                Ok(command) => terminals.handle_command(session_id, command),
                Err(error) => eprintln!("horizon-sessiond: malformed terminal command: {error}"),
            }
            continue;
        }

        let envelope = match agent_wire::decode_envelope(raw) {
            Ok(envelope) => envelope,
            Err(error) => {
                eprintln!("horizon-sessiond: unknown or malformed domain message: {error}");
                continue;
            }
        };

        match envelope.body {
            EnvelopeBody::Control(Control::SessionList) => {
                // Bind-first fix: block until `resume_persisted_sessions`
                // has finished, so a client that connects while it's still
                // running doesn't see an incomplete (or empty) session
                // list -- see the module doc and `SessiondState::
                // wait_until_resume_ready`.
                connection.wait_until_resume_ready().await;
                let _ = agent_outgoing_tx.send(Envelope::control(Control::SessionListResult(
                    connection.session_list(),
                )));
            }
            EnvelopeBody::Control(Control::SessionNew(new)) => {
                // Same readiness gate as `SessionList`/`SessionLoad` below,
                // for a different reason: `run_session`'s persistence choice
                // (`state.writer()` -- `LiveState::with_event_log_and_history`
                // vs. `with_disabled_persistence`) is decided once, at spawn
                // time. A `session_new` handled before `spawn_resume_task`
                // finishes calling `SessiondState::set_writer` would silently
                // spawn with persistence disabled for that session's entire
                // lifetime -- unnoticeable over the wire (folding/forwarding
                // happens either way) but permanently invisible to a later
                // `kill -9`/respawn or `session_load`. This race was narrow
                // enough to never trip in practice before `open_persistence`
                // grew an unconditional DuckDB rebuild ahead of `set_writer`
                // (the projection has no "disabled" state to opt into any
                // more); a client issuing `session_new` immediately after
                // connecting can now easily win it.
                connection.wait_until_resume_ready().await;
                connection.handle_session_new(new);
            }
            EnvelopeBody::Control(Control::SessionLoad(load)) => {
                // Same readiness gate as `SessionList` above: a resumed
                // session's thread may not exist yet while resume is still
                // in flight, which would otherwise make this replay as
                // "unknown session" (empty) instead of waiting for it.
                connection.wait_until_resume_ready().await;
                // Step 4's "v1 bootstrap": re-emit the fold-relevant
                // committed events for this session so the (re)connecting
                // client can rebuild its frame. Awaited inline (not spawned
                // detached) so these arrive before whatever the client sends
                // next for this session, keeping replay ordering simple.
                let events = connection.replay_events(load.session_id).await;
                for event in events {
                    let _ = agent_outgoing_tx.send(Envelope::event(load.session_id, event));
                }
                // Re-announces the session's resolved model to this
                // (re)attaching client -- it was already sent once, live, at
                // spawn time (`spawn_session_thread`), which this client
                // likely missed (it wasn't connected yet, e.g. a resumed
                // session at daemon startup, or a later reconnect). See
                // `docs/agent-output-ui-amendment.md`'s dated model-chip
                // addendum.
                if let Some(model) = connection.session_model(load.session_id) {
                    let _ = agent_outgoing_tx.send(Envelope {
                        v: CONTRACT_VERSION,
                        session_id: Some(load.session_id),
                        body: EnvelopeBody::Control(Control::SessionModel(model)),
                    });
                }
            }
            EnvelopeBody::Control(Control::HostToolResponse(response)) => {
                connection.handle_host_tool_response(response);
            }
            EnvelopeBody::Command(command) => match envelope.session_id {
                Some(session_id) => connection.route_command(session_id, command),
                None => {
                    eprintln!("horizon-sessiond: command envelope missing session_id, ignoring")
                }
            },
            other => {
                eprintln!("horizon-sessiond: unexpected message during session hosting: {other:?}");
            }
        }
    }
}

/// Blocks until every event-log record enqueued so far has actually been
/// written and flushed to disk (see [`WriterHandle::flush`]'s doc comment),
/// then returns -- called right before `std::process::exit(0)` on a
/// `SessionControl::Drain`. An `Appender::append_provider_events` call only
/// enqueues onto the writer's own background thread (see `WriterHandle::
/// open`'s "Ordering guarantee"); forwarding the resulting event to a
/// connected client happens after that same enqueue, not after it becomes
/// durable. Without this, a client observing a session's latest event over
/// the wire and immediately draining could still race the writer's thread
/// and lose it, or lose everything not yet drained -- indistinguishable
/// from the `kill -9` case this binary has no signal handler for, except a
/// graceful drain has every opportunity to just wait instead. A blocking
/// call is safe here despite running on an async task: this is the last
/// thing that happens before the process exits, so there is nothing else
/// for the runtime to make progress on.
fn flush_event_log_before_exit(writer: Option<WriterHandle>) {
    if let Some(writer) = writer {
        if let Err(error) = writer.flush() {
            eprintln!("horizon-sessiond: failed to flush event log before draining: {error}");
        }
    }
}

fn our_hello_envelope() -> Result<RawEnvelope, session_wire::WireError> {
    RawEnvelope::session_control(&SessionControl::Hello(Hello {
        contract_version: CONTRACT_VERSION,
        binary_id: BINARY_ID.to_string(),
    }))
}

/// Binds `path`, handling the stale-socket case: if a socket file already
/// exists there but nothing is accepting connections on it (a previous
/// `horizon-sessiond` that didn't shut down cleanly), remove it and rebind.
/// If something *is* accepting, refuses to steal the path out from under a
/// live instance.
async fn bind_listener(path: &Path) -> anyhow::Result<UnixListener> {
    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(_stream) => {
                anyhow::bail!(
                    "{} is already accepting connections -- is another horizon-sessiond running?",
                    path.display()
                );
            }
            Err(_) => {
                eprintln!(
                    "horizon-sessiond: removing stale socket {} (nothing was accepting)",
                    path.display()
                );
                std::fs::remove_file(path)?;
            }
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(UnixListener::bind(path)?)
}

fn socket_path_from_args<I: Iterator<Item = String>>(mut args: I) -> Option<PathBuf> {
    while let Some(arg) = args.next() {
        if arg == "--socket" {
            return args.next().map(PathBuf::from);
        }
        if let Some(value) = arg.strip_prefix("--socket=") {
            return Some(PathBuf::from(value));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_from_args_reads_the_space_separated_form() {
        let args = vec!["--socket".to_string(), "/tmp/sock".to_string()];
        assert_eq!(
            socket_path_from_args(args.into_iter()),
            Some(PathBuf::from("/tmp/sock"))
        );
    }

    #[test]
    fn socket_path_from_args_reads_the_equals_form() {
        let args = vec!["--socket=/tmp/sock2".to_string()];
        assert_eq!(
            socket_path_from_args(args.into_iter()),
            Some(PathBuf::from("/tmp/sock2"))
        );
    }

    #[test]
    fn socket_path_from_args_is_none_when_the_flag_is_absent() {
        let args: Vec<String> = vec!["--other-flag".to_string()];
        assert_eq!(socket_path_from_args(args.into_iter()), None);
    }
}
