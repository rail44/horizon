use std::sync::{Mutex, OnceLock};

use crossbeam_channel::unbounded;
use floem::ext_event::create_signal_from_channel;
use floem::prelude::*;
use floem::reactive::create_effect;

use crate::agent::config::{AgentConfig, AgentPersistenceConfig};
use crate::agent::persistence::event_log::{read, WriterHandle};
use crate::agent::persistence::projection::duckdb::Store;
use crate::agent::tools::{
    process_agent_provider_event, register_session_runtime, should_fold_completion, BashCompletion,
    ToolSessionState,
};
use crate::agent::{contract as agent, frame::AgentFrame, live::LiveState};
use crate::session::{Frames, Registry, SessionId};
use crate::workspace::Workspace;

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
static AGENT_DUCKDB_REBUILD_DONE: OnceLock<Mutex<bool>> = OnceLock::new();

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
) {
    let providers = agent::ProviderRegistry::builtin_with_config(agent_config.clone());
    let provider_id = providers.default_provider_id();
    let runtime_state = open_agent_runtime_state_store(
        session_id,
        provider_id.clone(),
        agent_state_status,
        &agent_config.persistence,
    );
    let Some(handle) = providers.start_session(&provider_id, session_id) else {
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

    let tool_state = ToolSessionState::for_current_dir();
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
        session_id,
        tool_state.clone(),
        runtime_state.clone(),
        bash_results_tx,
    );

    if let Some(sender) = sessions.with_untracked(|registry| registry.agent_sender(session_id)) {
        let _ = sender.send(agent::Command::Initialize(agent::Initialization {
            session_id,
            provider_id: provider_id.clone(),
        }));
    }

    let bash_runtime_state = runtime_state.clone();
    create_effect(move |_| {
        if let Some(event) = events.get() {
            let processing =
                workspace.with_untracked(|ws| process_agent_provider_event(ws, &tool_state, event));
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
    let event_log = match open_agent_event_log(persistence_config) {
        Ok((writer, status)) => {
            let mut messages = Vec::new();
            if let Some(status) = status {
                messages.push(status);
            }
            match rebuild_agent_duckdb_from_event_log_once(persistence_config) {
                Ok(Some(skipped_summary)) => {
                    messages.push(format!("Agent event log: {skipped_summary}"));
                }
                Ok(None) => {}
                Err(error) => {
                    messages.push(format!(
                        "Agent DuckDB projection rebuild unavailable: {error}"
                    ));
                }
            }
            if !messages.is_empty() {
                agent_state_status.set(Some(messages.join(" | ")));
            }
            Some(writer)
        }
        Err(error) => {
            agent_state_status.set(Some(format!(
                "Agent event log unavailable ({error}); persistence disabled"
            )));
            None
        }
    };

    if let Some(event_log) = event_log {
        LiveState::with_event_log(session_id, Some(provider_id), event_log)
    } else {
        LiveState::with_disabled_persistence()
    }
}

fn open_agent_event_log(
    persistence_config: &AgentPersistenceConfig,
) -> anyhow::Result<(WriterHandle, Option<String>)> {
    let writer_cell = AGENT_EVENT_LOG_WRITER.get_or_init(|| Mutex::new(None));
    let mut writer = writer_cell
        .lock()
        .map_err(|_| anyhow::anyhow!("agent event log writer lock poisoned"))?;
    if let Some(writer) = writer.as_ref() {
        return Ok((writer.clone(), None));
    }

    let path = persistence_config.event_log_path.clone();
    let status = Some(format!("Agent event log: {}", path.display()));
    let handle = WriterHandle::open(path)?;
    *writer = Some(handle.clone());
    Ok((handle, status))
}

/// Runs [`rebuild_agent_duckdb_from_event_log`] at most once per process.
/// Returns the skipped-line summary from that one rebuild (`Ok(None)` when
/// either the rebuild was already done by an earlier call, or it ran and
/// found nothing to skip).
fn rebuild_agent_duckdb_from_event_log_once(
    persistence_config: &AgentPersistenceConfig,
) -> anyhow::Result<Option<String>> {
    let rebuild_done = AGENT_DUCKDB_REBUILD_DONE.get_or_init(|| Mutex::new(false));
    let mut rebuild_done = rebuild_done
        .lock()
        .map_err(|_| anyhow::anyhow!("agent DuckDB rebuild lock poisoned"))?;
    if *rebuild_done {
        return Ok(None);
    }

    let skipped_summary = rebuild_agent_duckdb_from_event_log(persistence_config)?;
    *rebuild_done = true;
    Ok(skipped_summary)
}

/// Replays the JSONL event log into the DuckDB projection. `event_log::read`
/// already skips corrupt/torn lines rather than failing (see
/// `ReadReport::skipped_summary`); this only adds a warning-style log so the
/// skip isn't silently swallowed, and hands the summary back so the caller
/// can surface it in the UI status line too.
fn rebuild_agent_duckdb_from_event_log(
    persistence_config: &AgentPersistenceConfig,
) -> anyhow::Result<Option<String>> {
    let Some(db_path) = persistence_config.duckdb_path.clone() else {
        return Ok(None);
    };
    let log_path = persistence_config.event_log_path.clone();

    let report = read(&log_path)?;
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

        let (first, first_status) =
            open_agent_event_log(&persistence_config).expect("open event log");
        assert!(
            first_status.is_some(),
            "the session that actually opens the file should report where it lives"
        );

        let (second, second_status) =
            open_agent_event_log(&persistence_config).expect("reopen event log");
        assert!(
            second_status.is_none(),
            "a cached reuse should not repeat the status message"
        );
        assert!(
            first.same_channel(&second),
            "two sessions in one process must share one writer thread, not \
             create a second writer racing on the same file"
        );

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
        let session_id = SessionId::new();
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

        let skipped_summary = rebuild_agent_duckdb_from_event_log(&persistence_config)
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
}
