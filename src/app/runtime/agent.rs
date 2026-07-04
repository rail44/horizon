use std::sync::{Mutex, OnceLock};
use std::thread;

use crossbeam_channel::{unbounded, Receiver};
use floem::ext_event::create_signal_from_channel;
use floem::prelude::*;
use floem::reactive::create_effect;

use crate::agent::agentd_runtime::AgentdConnection;
use crate::agent::config::{AgentConfig, AgentPersistenceConfig};
use crate::agent::persistence::event_log::{read, ReadReport, WriterHandle, WriterInit};
use crate::agent::persistence::projection::duckdb::Store;
use crate::agent::tools::{
    process_agent_provider_event, register_session_runtime, should_fold_completion, BashCompletion,
    ToolSessionState,
};
use crate::agent::{contract as agent, frame::AgentFrame, live::LiveState, WorkspaceHostTools};
use crate::session::{Frames, Registry, SessionId};
use crate::workspace::Workspace;

#[cfg(test)]
thread_local! {
    /// Test-only call counter for [`open_agent_event_log`] -- the seam-level
    /// proof that agentd mode never opens Horizon's own copy of the event
    /// log (decision 3, "Persistence moves": agentd owns the writer,
    /// Horizon must not double-write). Two things make a naive "did it get
    /// called" check harder than it looks, both handled here:
    ///
    /// `AGENT_EVENT_LOG_WRITER` below is a genuinely process-global
    /// `OnceLock` shared by every test in this binary, so asserting "the
    /// log file at this fresh path was never created" is not reliable on
    /// its own (an earlier test may have already warmed the cache against
    /// a *different* path, in which case a wrongly reintroduced call here
    /// would silently reuse that other writer instead of ever touching
    /// ours) -- counting calls to the function that owns the cache
    /// sidesteps that.
    ///
    /// `cargo test`'s default harness runs tests concurrently on separate
    /// threads, so a plain process-global counter would still be flaky:
    /// another test calling `open_agent_event_log` between this test's
    /// "before" read and its assertion would look identical to a real
    /// regression. `thread_local!` avoids that too -- every real call in
    /// this test happens synchronously on the test's own thread
    /// (`spawn_agent_session_via_agentd`'s only spawned thread just
    /// forwards commands over a channel, never touches this), so counting
    /// per-thread makes the result independent of whatever other tests are
    /// doing concurrently.
    static OPEN_AGENT_EVENT_LOG_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn open_agent_event_log_call_count() -> usize {
    OPEN_AGENT_EVENT_LOG_CALLS.with(std::cell::Cell::get)
}

/// The process-global cache that makes this the *only* call site that
/// constructs a [`WriterHandle`] outside of tests (see [`open_agent_event_log`]).
///
/// Design choice for "eliminate concurrent same-file appends": a single
/// shared writer per process, rather than one JSONL file per session.
///
/// - **Chosen — process-global shared writer.** `open_agent_event_log`
///   opens the event log file at most once per process and every agent
///   session spawned afterwards (`spawn_agent_session`, regardless of how
///   many panes/tabs are opened or how close together) reuses the same
///   [`WriterHandle`] clone. A `WriterHandle` is one background thread
///   holding one open `File` and one `BufWriter`; cloning it only clones a
///   channel `Sender`, so every session's appends funnel through the same
///   thread and get serialized by the channel itself — concurrent writers
///   to the file become structurally impossible within this process. This
///   is precisely the bug that produced the torn lines in
///   `/tmp/horizon-agent-events.jsonl`: two sessions opened moments apart
///   each got their own `WriterHandle` (own thread, own `File`), and their
///   independent buffered writes interleaved on disk.
/// - **Rejected — per-session log files.** Giving each session its own file
///   (e.g. `agent-events-<session_id>.jsonl`) also prevents same-file
///   races, but pushes the complexity onto the read side: `event_log::read`
///   and the DuckDB replay below would need to discover every file in a
///   directory, open each one, and merge-sort records across files by
///   `sequence`/`created_at_unix_ms` instead of reading one file top to
///   bottom. The shared-writer design keeps that path exactly as simple as
///   it is today — a single `read(path)` — at the cost of a small
///   process-global cache here.
///
/// Caveat: this guards against concurrent writers *within this process*.
/// Horizon has no single-instance enforcement, so two separate OS processes
/// both pointed at the same `event_log_path` could still race; that is not
/// the failure mode the evidence pointed to (two sessions, one process) and
/// is out of scope here.
static AGENT_EVENT_LOG_WRITER: OnceLock<Mutex<Option<WriterHandle>>> = OnceLock::new();

/// Flushes the process-global writer (if one was ever opened) so that
/// records enqueued but not yet written survive a normal app exit. Wired
/// from `main.rs` via floem's `AppEvent::WillTerminate` through
/// `app::shutdown` — see that call chain for why this can't just be a
/// `Drop` impl: the writer lives in a `OnceLock` static, which is never
/// dropped when `main` returns normally.
///
/// A hard kill (SIGKILL, crash) bypasses this entirely and can still leave
/// a torn final line on disk; `event_log::read` tolerates that rather than
/// failing replay (see `ReadReport::ignored_partial_line`).
pub(crate) fn shutdown_agent_event_log() {
    let Some(writer_cell) = AGENT_EVENT_LOG_WRITER.get() else {
        return;
    };
    let Ok(writer) = writer_cell.lock() else {
        return;
    };
    if let Some(writer) = writer.as_ref() {
        if let Err(error) = writer.flush() {
            eprintln!("horizon agent event log: shutdown flush failed: {error}");
        }
    }
}

pub(super) fn spawn_agent_session(
    session_id: SessionId,
    workspace: RwSignal<Workspace>,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    agent_state_status: RwSignal<Option<String>>,
    agent_config: AgentConfig,
    agentd_connection: Option<&AgentdConnection>,
) {
    // Step 3 routing (`docs/agent-runtime-split-design.md`): when Horizon
    // successfully connected to `horizon-agentd` at startup, every agent
    // session hosts there instead -- tools, approvals, and persistence all
    // move with it (see `spawn_agent_session_via_agentd`'s doc comment).
    // `agentd_connection` is `None` both when `[agent].agentd` is off (the
    // default) and when the flag was on but the startup connection failed,
    // so this one check keeps every existing in-process call below
    // completely unchanged for both of those cases.
    if let Some(connection) = agentd_connection {
        spawn_agent_session_via_agentd(session_id, frames, sessions, connection);
        return;
    }

    let providers = agent::ProviderRegistry::builtin_with_config(agent_config.clone());
    let provider_id = providers.default_provider_id();
    let runtime_state = open_agent_runtime_state_store(
        session_id,
        provider_id.clone(),
        agent_state_status,
        &agent_config.persistence,
    );
    let Some(handle) = providers.start_session(&provider_id, session_id.into()) else {
        frames.update(|frames| {
            frames.update_agent_frame(
                session_id,
                AgentFrame {
                    state: None,
                    items: Vec::new(),
                },
            );
        });
        return;
    };
    let events = create_signal_from_channel(handle.events());
    sessions.update(|registry| {
        registry.insert_agent(session_id, handle);
    });

    let tool_state = ToolSessionState::for_current_dir(agent_config.tools);
    // `bash` calls run on a dedicated background thread and can take up to
    // their timeout, so their result can't fold into `LiveState`/`Frames`
    // synchronously the way `fs.write`/`fs.edit` do (those are UI-thread-
    // confined — see `agent::live::LiveState`). This channel is the
    // cross-thread-to-UI delivery seam for that result: the same
    // `crossbeam_channel` + `create_signal_from_channel` bridge used just
    // above for `handle.events()`, the provider's own event stream.
    let (bash_results_tx, bash_results_rx) = unbounded::<BashCompletion>();
    let bash_completions = create_signal_from_channel(bash_results_rx);
    register_session_runtime(
        session_id.into(),
        tool_state.clone(),
        runtime_state.clone(),
        bash_results_tx,
    );

    if let Some(sender) = sessions.with_untracked(|registry| registry.agent_sender(session_id)) {
        let _ = sender.send(agent::Command::Initialize(agent::Initialization {
            session_id: session_id.into(),
            provider_id: provider_id.clone(),
        }));
    }

    let bash_runtime_state = runtime_state.clone();
    create_effect(move |_| {
        if let Some(event) = events.get() {
            let processing = workspace.with_untracked(|ws| {
                process_agent_provider_event(&WorkspaceHostTools(ws), &tool_state, event)
            });
            for command in processing.provider_commands {
                if let Some(sender) =
                    sessions.with_untracked(|registry| registry.agent_sender(session_id))
                {
                    let _ = sender.send(command);
                }
            }
            let frame = runtime_state.extend_provider_events(processing.horizon_events);
            frames.update(|frames| frames.update_agent_frame(session_id, frame));
        }
    });

    create_effect(move |_| {
        if let Some(completion) = bash_completions.get() {
            fold_bash_completion(
                &bash_runtime_state,
                frames,
                sessions,
                session_id,
                completion,
            );
        }
    });
}

/// The agentd-routed counterpart of `spawn_agent_session` above (step 3):
/// tool execution, approval resolution, and persistence all moved into
/// `horizon-agentd` (see `horizon-agentd`'s `session` module), so this side
/// only has to do what's left -- ask agentd to host the session
/// (`AgentdConnection::start_session`, which sends `session_new` and hands
/// back a `SessionHandle` indistinguishable from an in-process one at every
/// other call site) and fold the resulting event stream into the frame the
/// pane renders.
///
/// That fold is deliberately the *same* `LiveState::extend_provider_events`
/// and `Frames::update_agent_frame` step `spawn_agent_session`'s own effect
/// uses — "the fold must not know which transport delivered the events" —
/// just without the `process_agent_provider_event` call in front of it: the
/// events arriving over the wire already went through that pipeline in
/// agentd, so running it again here would re-execute already-executed tool
/// calls. `LiveState::with_disabled_persistence()` is deliberate too, not a
/// placeholder: decision 3 in `docs/agent-runtime-split-design.md` ("the
/// child owns the event log") means Horizon must not append its own copy —
/// see [`open_agent_event_log_call_count`]'s doc comment for how that
/// guarantee is tested, and note this function never calls
/// `open_agent_runtime_state_store`/`open_agent_event_log` at all, unlike
/// the in-process path above.
pub(super) fn spawn_agent_session_via_agentd(
    session_id: SessionId,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    connection: &AgentdConnection,
) {
    let provider_id = agent::ProviderRegistry::default().default_provider_id();
    let handle = connection.start_session(session_id.into(), provider_id);
    let events = create_signal_from_channel(handle.events());
    sessions.update(|registry| {
        registry.insert_agent(session_id, handle);
    });

    let runtime_state = LiveState::with_disabled_persistence();
    create_effect(move |_| {
        if let Some(event) = events.get() {
            let frame = runtime_state.extend_provider_events(std::iter::once(event));
            frames.update(|frames| frames.update_agent_frame(session_id, frame));
        }
    });
}

/// Folds a finished bash call's result into the session's frame and
/// forwards it to the provider — the async-execution analogue of
/// `agent::tools::approval::ApprovalOutcome::Executed`'s synchronous fold,
/// which the click handler in `workspace/view/pane.rs` does directly.
///
/// Guards against double-folding a late result the same way `agent::tools::
/// approval` guards a double approve/deny: if the call already has a
/// `ToolCallFinished` — because a turn cancellation raced this completion
/// and got there first (see `agent::tools::processing::
/// process_agent_provider_event`, which kills the child but can't stop a
/// result that was already in flight on this channel) — the result is
/// accepted and discarded here, matching the provider's own contract for a
/// `ToolCallResult` arriving after cancel.
fn fold_bash_completion(
    runtime_state: &LiveState,
    frames: RwSignal<Frames>,
    sessions: RwSignal<Registry>,
    session_id: SessionId,
    completion: BashCompletion,
) {
    let result = completion.result;
    if !should_fold_completion(&runtime_state.frame(), &result.call_id) {
        return;
    }

    let events = [
        agent::Event::ToolCallFinished(result.clone()),
        agent::Event::StateChanged(agent::SessionState::WaitingForUser),
    ];
    let frame = runtime_state.extend_provider_events(events.into_iter().map(Into::into));
    frames.update(|frames| frames.update_agent_frame(session_id, frame));

    if let Some(sender) = sessions.with_untracked(|registry| registry.agent_sender(session_id)) {
        let _ = sender.send(agent::Command::ToolCallResult(result));
    }
}

fn open_agent_runtime_state_store(
    session_id: SessionId,
    provider_id: agent::ProviderId,
    agent_state_status: RwSignal<Option<String>>,
    persistence_config: &AgentPersistenceConfig,
) -> LiveState {
    match open_agent_event_log(persistence_config) {
        Ok((writer, init_rx)) => {
            if let Some(init_rx) = init_rx {
                spawn_persistence_initialization(
                    persistence_config.clone(),
                    agent_state_status,
                    init_rx,
                );
            }
            LiveState::with_event_log(session_id.into(), Some(provider_id), writer)
        }
        Err(error) => {
            agent_state_status.set(Some(format!(
                "Agent event log unavailable ({error}); persistence disabled"
            )));
            LiveState::with_disabled_persistence()
        }
    }
}

/// Opens the process-global event log writer, returning the receiver for
/// its one-time startup-read outcome ([`WriterInit`]) — but only on the
/// call that actually creates the writer (`Some`); a call that hits the
/// cache (`None`) reused an already-open writer and has no new
/// initialization to observe. `WriterHandle::open` never blocks the caller
/// on the read itself (see its doc comment for the ordering guarantee), so
/// this function — and therefore `open_agent_runtime_state_store` and
/// `spawn_agent_session` above it — never performs the read on the
/// caller's thread either. The one caller that gets `Some`,
/// `open_agent_runtime_state_store`, hands the receiver to
/// `spawn_persistence_initialization` to drive the DuckDB replay and the
/// `agent_state_status` update once the background read completes.
fn open_agent_event_log(
    persistence_config: &AgentPersistenceConfig,
) -> anyhow::Result<(WriterHandle, Option<Receiver<WriterInit>>)> {
    #[cfg(test)]
    OPEN_AGENT_EVENT_LOG_CALLS.with(|calls| calls.set(calls.get() + 1));

    let writer_cell = AGENT_EVENT_LOG_WRITER.get_or_init(|| Mutex::new(None));
    let mut writer = writer_cell
        .lock()
        .map_err(|_| anyhow::anyhow!("agent event log writer lock poisoned"))?;
    if let Some(writer) = writer.as_ref() {
        return Ok((writer.clone(), None));
    }

    let path = persistence_config.event_log_path.clone();
    let (handle, init_rx) = WriterHandle::open(path);
    *writer = Some(handle.clone());
    Ok((handle, Some(init_rx)))
}

/// Kicked off exactly once per process, by the one call to
/// `open_agent_event_log` that actually creates the writer (see its doc
/// comment) — i.e. the process's first agent pane. Shows a "catching up"
/// status immediately (synchronously, on the caller's thread — the UI
/// thread, for every real caller, since `RwSignal::set` must only ever be
/// touched from there) and then hands the rest off to a background
/// thread: waiting for the writer's startup read to finish and running the
/// DuckDB replay (`finish_persistence_initialization`), which can itself
/// be expensive for a large accumulated history. The result is bridged
/// back with `create_signal_from_channel` + `create_effect` — the same
/// cross-thread-to-UI pattern `spawn_agent_session` uses for provider
/// events and bash completions — so the eventual `agent_state_status`
/// update also happens on the UI thread. A failed startup read is
/// surfaced the same way instead of silently leaving persistence broken
/// with no explanation.
fn spawn_persistence_initialization(
    persistence_config: AgentPersistenceConfig,
    agent_state_status: RwSignal<Option<String>>,
    init_rx: Receiver<WriterInit>,
) {
    agent_state_status.set(Some(format!(
        "Agent persistence: catching up on {}",
        persistence_config.event_log_path.display()
    )));

    let (outcome_tx, outcome_rx) = unbounded::<Option<String>>();
    thread::spawn(move || {
        let Ok(init) = init_rx.recv() else {
            // The writer's background thread is gone without ever sending
            // an outcome (it would have to have panicked before reaching
            // either `send` in `WriterHandle::open_with_reader`). Leave
            // whatever status is currently showing rather than guessing.
            return;
        };
        let outcome = resolve_persistence_init_outcome(&persistence_config, init);
        let _ = outcome_tx.send(outcome);
    });

    let outcome_signal = create_signal_from_channel(outcome_rx);
    create_effect(move |_| {
        if let Some(outcome) = outcome_signal.get() {
            agent_state_status.set(outcome);
        }
    });
}

/// Maps a [`WriterInit`] outcome to the `agent_state_status` message that
/// should replace `spawn_persistence_initialization`'s "catching up"
/// status: `None` clears it (nothing to report), `Some(..)` replaces it
/// with a skipped-lines/rebuild-failure/read-failure message. Kept
/// separate from `spawn_persistence_initialization` so both branches are
/// directly testable without any floem or thread involved.
fn resolve_persistence_init_outcome(
    persistence_config: &AgentPersistenceConfig,
    init: WriterInit,
) -> Option<String> {
    match init {
        WriterInit::Ready(report) => finish_persistence_initialization(persistence_config, report),
        WriterInit::Failed(error) => Some(format!(
            "Agent event log unavailable ({error}); persistence disabled"
        )),
    }
}

/// The actual one-time initialization work for the success path: replays
/// the event log's `ReadReport` (already produced by the writer's startup
/// read — see `WriterInit::Ready`) into the DuckDB projection via
/// `rebuild_agent_duckdb_from_event_log`, and summarizes the outcome as
/// described on [`resolve_persistence_init_outcome`].
fn finish_persistence_initialization(
    persistence_config: &AgentPersistenceConfig,
    report: ReadReport,
) -> Option<String> {
    match rebuild_agent_duckdb_from_event_log(persistence_config, Some(report)) {
        Ok(Some(skipped_summary)) => Some(format!("Agent event log: {skipped_summary}")),
        Ok(None) => None,
        Err(error) => Some(format!(
            "Agent DuckDB projection rebuild unavailable: {error}"
        )),
    }
}

/// Replays the JSONL event log into the DuckDB projection, from `report` if
/// the caller already read one (the normal case — see
/// `finish_persistence_initialization`) or by reading the file itself
/// otherwise. `event_log::read` already skips corrupt/torn lines rather
/// than failing (see `ReadReport::skipped_summary`); this only adds a
/// warning-style log so the skip isn't silently swallowed, and hands the
/// summary back so the caller can surface it in the UI status line too.
fn rebuild_agent_duckdb_from_event_log(
    persistence_config: &AgentPersistenceConfig,
    initial_read: Option<ReadReport>,
) -> anyhow::Result<Option<String>> {
    let Some(db_path) = persistence_config.duckdb_path.clone() else {
        return Ok(None);
    };
    let log_path = &persistence_config.event_log_path;

    let report = match initial_read {
        Some(report) => report,
        None => read(log_path)?,
    };
    let skipped_summary = report.skipped_summary();
    if let Some(summary) = &skipped_summary {
        eprintln!(
            "horizon agent event log: {summary} while replaying {}",
            log_path.display()
        );
    }

    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let store = Store::open(db_path)?;
    store.replace_from_event_log_records(report.records)?;
    Ok(skipped_summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::{Event, Message, MessageRole};
    use crate::agent::persistence::event_log::{
        Record, AGENT_EVENT_LOG_SCHEMA, AGENT_EVENT_LOG_VERSION,
    };
    use uuid::Uuid;

    /// Seam-level proof for step 3's "no double-write" requirement
    /// (`docs/agent-runtime-split-design.md`, "Persistence moves"): when
    /// `spawn_agent_session` is given a live `agentd_connection`, it must
    /// route to `spawn_agent_session_via_agentd` and never call
    /// `open_agent_event_log` -- persistence is owned entirely by agentd in
    /// that mode. Asserted via a call counter rather than checking whether
    /// some fresh path's file got created, because `AGENT_EVENT_LOG_WRITER`
    /// is a real process-global `OnceLock` shared by every test in this
    /// binary (see `open_agent_event_log_reuses_process_global_writer`
    /// just below) -- a file-existence check could pass for the wrong
    /// reason if an earlier test already warmed that cache against a
    /// different path. Counting calls to the function that owns the cache
    /// is unaffected by that.
    #[test]
    fn agentd_mode_never_opens_horizons_own_event_log() {
        let calls_before = open_agent_event_log_call_count();
        let connection = AgentdConnection::for_test();

        spawn_agent_session(
            SessionId::new(),
            RwSignal::new(Workspace::mvp()),
            RwSignal::new(Frames::default()),
            RwSignal::new(Registry::default()),
            RwSignal::new(None),
            AgentConfig::from_env_and_file(&crate::agent::config::AgentFileConfig::default()),
            Some(&connection),
        );

        assert_eq!(
            open_agent_event_log_call_count(),
            calls_before,
            "agentd mode must not open Horizon's own copy of the event log -- persistence is \
             owned entirely by horizon-agentd in this mode"
        );
    }

    /// Regression test for the actual corruption incident: two sessions
    /// opening the event log must not each get their own writer thread.
    /// `open_agent_event_log` is the process-global cache described on
    /// `AGENT_EVENT_LOG_WRITER`; this asserts a second call reuses the
    /// first session's `WriterHandle` (same background thread) instead of
    /// silently constructing a second one that would race on the file.
    #[test]
    fn open_agent_event_log_reuses_process_global_writer() {
        let path = std::env::temp_dir().join(format!(
            "horizon-agent-open-event-log-{}.jsonl",
            Uuid::new_v4()
        ));
        let persistence_config = AgentPersistenceConfig {
            event_log_path: path.clone(),
            duckdb_path: None,
        };

        let (first, first_init_rx) =
            open_agent_event_log(&persistence_config).expect("open event log");
        let first_init_rx = first_init_rx.expect(
            "the call that actually opens the file should get a receiver for its \
             startup-read outcome",
        );

        let (second, second_init_rx) =
            open_agent_event_log(&persistence_config).expect("reopen event log");
        assert!(
            second_init_rx.is_none(),
            "a cached reuse did no new initialization, so it has no outcome to hand back"
        );
        assert!(
            first.same_channel(&second),
            "two sessions in one process must share one writer thread, not \
             create a second writer racing on the same file"
        );

        match first_init_rx.recv().expect("writer init outcome") {
            WriterInit::Ready(_) => {}
            WriterInit::Failed(error) => panic!("unexpected startup failure: {error}"),
        }

        let _ = std::fs::remove_file(path);
    }

    /// The DuckDB replay path must not fail outright when the JSONL has
    /// corrupt or torn lines — it should skip them (via `event_log::read`)
    /// and report how many, so the caller can surface a warning instead of
    /// losing the whole session history.
    #[test]
    fn rebuild_agent_duckdb_from_event_log_reports_skipped_lines() {
        let log_path = std::env::temp_dir().join(format!(
            "horizon-agent-rebuild-log-{}.jsonl",
            Uuid::new_v4()
        ));
        let db_path =
            std::env::temp_dir().join(format!("horizon-agent-rebuild-{}.duckdb", Uuid::new_v4()));
        let session_id = agent::SessionId::new();
        let record = Record {
            schema: AGENT_EVENT_LOG_SCHEMA.to_string(),
            version: AGENT_EVENT_LOG_VERSION,
            event_id: "event-1".to_string(),
            sequence: 0,
            session_id,
            turn_id: None,
            provider_id: None,
            event_kind: "message_committed".to_string(),
            event: Event::MessageCommitted(Message {
                role: MessageRole::User,
                text: "hello".to_string(),
            }),
            provider_payload: None,
            created_at_unix_ms: 1,
        };
        let contents = format!(
            "{}\n{}\n{}",
            serde_json::to_string(&record).expect("serialize record"),
            "not valid json",
            "{\"schema\":\"horizon.agent.event_log\",\"version\":1,\"event_id\":\"torn-tail\"",
        );
        std::fs::write(&log_path, contents).expect("write fixture log");

        let persistence_config = AgentPersistenceConfig {
            event_log_path: log_path.clone(),
            duckdb_path: Some(db_path.clone()),
        };

        // No `initial_read` supplied: exercises the defensive fallback that
        // reads the file itself (see the doc comment on
        // `rebuild_agent_duckdb_from_event_log`).
        let skipped_summary = rebuild_agent_duckdb_from_event_log(&persistence_config, None)
            .expect("rebuild from event log");
        assert_eq!(
            skipped_summary.as_deref(),
            Some("skipped 1 corrupt line and a torn trailing line")
        );

        let store = Store::open(&db_path).expect("reopen duckdb store");
        let sessions = store.sessions().expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, session_id);

        let _ = std::fs::remove_file(log_path);
        let _ = std::fs::remove_file(db_path);
    }

    /// The whole point of threading `ReadReport` through: when the caller
    /// already read the log (as `open_agent_event_log` does via
    /// `WriterHandle::open`), the rebuild must use those records rather
    /// than reading the file again. Proven here by deleting the file before
    /// calling the rebuild — `event_log::read` treats a missing file as
    /// "no records" rather than an error, so a stray second read would
    /// silently succeed with zero records instead of failing loudly; the
    /// only way this test's session ends up in the rebuilt DuckDB store is
    /// if the passed-through report was used instead.
    #[test]
    fn rebuild_agent_duckdb_from_event_log_uses_passed_through_report_without_rereading_the_file() {
        let log_path = std::env::temp_dir().join(format!(
            "horizon-agent-rebuild-no-reread-log-{}.jsonl",
            Uuid::new_v4()
        ));
        let db_path = std::env::temp_dir().join(format!(
            "horizon-agent-rebuild-no-reread-{}.duckdb",
            Uuid::new_v4()
        ));
        let session_id = agent::SessionId::new();
        let record = Record {
            schema: AGENT_EVENT_LOG_SCHEMA.to_string(),
            version: AGENT_EVENT_LOG_VERSION,
            event_id: "event-1".to_string(),
            sequence: 0,
            session_id,
            turn_id: None,
            provider_id: None,
            event_kind: "message_committed".to_string(),
            event: Event::MessageCommitted(Message {
                role: MessageRole::User,
                text: "hello".to_string(),
            }),
            provider_payload: None,
            created_at_unix_ms: 1,
        };
        let initial_read = crate::agent::persistence::event_log::ReadReport {
            records: vec![record],
            corrupt_line_count: 0,
            ignored_partial_line: false,
        };

        // Deliberately never written, so any re-read would see "file does
        // not exist" and silently come back with zero records.
        assert!(!log_path.exists());

        let persistence_config = AgentPersistenceConfig {
            event_log_path: log_path.clone(),
            duckdb_path: Some(db_path.clone()),
        };

        let skipped_summary =
            rebuild_agent_duckdb_from_event_log(&persistence_config, Some(initial_read))
                .expect("rebuild from passed-through report");
        assert_eq!(skipped_summary, None);

        let store = Store::open(&db_path).expect("reopen duckdb store");
        let sessions = store.sessions().expect("sessions");
        assert_eq!(
            sessions.len(),
            1,
            "the passed-through report's session should have made it into the \
             rebuilt store even though the log file was never on disk"
        );
        assert_eq!(sessions[0].session_id, session_id);

        let _ = std::fs::remove_file(db_path);
    }

    /// The success half of the background initialization pipeline: once
    /// the writer's startup read hands back a clean `ReadReport`,
    /// `finish_persistence_initialization` must both project its records
    /// into DuckDB and clear the "catching up" status (`None`) rather than
    /// leaving it showing forever.
    #[test]
    fn finish_persistence_initialization_projects_records_and_clears_status_when_clean() {
        let db_path = std::env::temp_dir().join(format!(
            "horizon-agent-finish-init-{}.duckdb",
            Uuid::new_v4()
        ));
        let session_id = agent::SessionId::new();
        let record = Record {
            schema: AGENT_EVENT_LOG_SCHEMA.to_string(),
            version: AGENT_EVENT_LOG_VERSION,
            event_id: "event-1".to_string(),
            sequence: 0,
            session_id,
            turn_id: None,
            provider_id: None,
            event_kind: "message_committed".to_string(),
            event: Event::MessageCommitted(Message {
                role: MessageRole::User,
                text: "hello".to_string(),
            }),
            provider_payload: None,
            created_at_unix_ms: 1,
        };
        let report = crate::agent::persistence::event_log::ReadReport {
            records: vec![record],
            corrupt_line_count: 0,
            ignored_partial_line: false,
        };
        let persistence_config = AgentPersistenceConfig {
            // Never written to disk: `report` is passed through directly,
            // exactly as `spawn_persistence_initialization` does with the
            // writer's own `WriterInit::Ready` report.
            event_log_path: std::env::temp_dir().join(format!(
                "horizon-agent-finish-init-log-{}.jsonl",
                Uuid::new_v4()
            )),
            duckdb_path: Some(db_path.clone()),
        };

        let status = finish_persistence_initialization(&persistence_config, report);
        assert_eq!(
            status, None,
            "a clean rebuild should clear the catching-up status, not leave a stale message"
        );

        let store = Store::open(&db_path).expect("reopen duckdb store");
        let sessions = store.sessions().expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, session_id);

        let _ = std::fs::remove_file(db_path);
    }

    /// Skipped lines in the startup read must still surface as a status
    /// message rather than disappearing once the rebuild completes.
    #[test]
    fn finish_persistence_initialization_surfaces_skipped_lines_in_status() {
        let db_path = std::env::temp_dir().join(format!(
            "horizon-agent-finish-init-skips-{}.duckdb",
            Uuid::new_v4()
        ));
        let persistence_config = AgentPersistenceConfig {
            event_log_path: std::env::temp_dir().join(format!(
                "horizon-agent-finish-init-skips-log-{}.jsonl",
                Uuid::new_v4()
            )),
            duckdb_path: Some(db_path.clone()),
        };
        let report = crate::agent::persistence::event_log::ReadReport {
            records: Vec::new(),
            corrupt_line_count: 2,
            ignored_partial_line: true,
        };

        let status = finish_persistence_initialization(&persistence_config, report);
        assert_eq!(
            status.as_deref(),
            Some("Agent event log: skipped 2 corrupt lines and a torn trailing line")
        );

        let _ = std::fs::remove_file(db_path);
    }

    /// A DuckDB rebuild failure (here: the configured path is a directory,
    /// so `Store::open` can't open it as a database file) must surface in
    /// the status instead of failing silently.
    #[test]
    fn finish_persistence_initialization_surfaces_duckdb_rebuild_failures_in_status() {
        let db_path = std::env::temp_dir().join(format!(
            "horizon-agent-finish-init-bad-db-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&db_path).expect("create directory standing in for the db path");
        let persistence_config = AgentPersistenceConfig {
            event_log_path: std::env::temp_dir().join(format!(
                "horizon-agent-finish-init-bad-db-log-{}.jsonl",
                Uuid::new_v4()
            )),
            duckdb_path: Some(db_path.clone()),
        };

        let status = finish_persistence_initialization(
            &persistence_config,
            crate::agent::persistence::event_log::ReadReport::default(),
        );
        assert!(
            matches!(
                &status,
                Some(message) if message.starts_with("Agent DuckDB projection rebuild unavailable:")
            ),
            "a rebuild failure must surface in the status instead of failing silently, got {status:?}"
        );

        let _ = std::fs::remove_dir_all(db_path);
    }

    /// The other half of `resolve_persistence_init_outcome`: a startup-read
    /// failure must surface in the status too, not just rebuild failures.
    #[test]
    fn resolve_persistence_init_outcome_surfaces_startup_read_failures_in_status() {
        let persistence_config = AgentPersistenceConfig {
            event_log_path: std::env::temp_dir().join("horizon-agent-unused.jsonl"),
            duckdb_path: None,
        };

        let status = resolve_persistence_init_outcome(
            &persistence_config,
            WriterInit::Failed(anyhow::anyhow!("permission denied")),
        );
        assert_eq!(
            status.as_deref(),
            Some("Agent event log unavailable (permission denied); persistence disabled")
        );
    }

    /// `spawn_persistence_initialization` must show a "catching up" status
    /// immediately, synchronously, on the caller's thread — the whole point
    /// is that a session can render its pane right away while background
    /// initialization is still in flight, rather than the status bar
    /// staying blank (or worse, the caller blocking) until it finishes.
    /// The receiver here is never sent to, so this only exercises the
    /// synchronous part; the background thread just waits forever, which
    /// is harmless for the lifetime of this test.
    #[test]
    fn spawn_persistence_initialization_immediately_shows_a_catching_up_status() {
        let agent_state_status: RwSignal<Option<String>> = RwSignal::new(None);
        let persistence_config = AgentPersistenceConfig {
            event_log_path: std::env::temp_dir().join("horizon-agent-catching-up.jsonl"),
            duckdb_path: None,
        };
        let (_init_tx, init_rx) = crossbeam_channel::unbounded::<WriterInit>();

        spawn_persistence_initialization(persistence_config, agent_state_status, init_rx);

        let status = agent_state_status.get_untracked();
        assert!(
            status
                .as_deref()
                .is_some_and(|status| status.contains("catching up")),
            "opening the writer should immediately surface a catching-up status instead \
             of leaving the status bar silent while the background read/rebuild runs, \
             got {status:?}"
        );
    }
}
