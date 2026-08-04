//! The rig session: spawns the OS thread, loads history, and drives the
//! [`SessionLoopState`] turn loop. Split into:
//!
//! - [`mod@state`] — [`SessionLoopState`] (the mutable loop state bundled into
//!   a struct) and the loop body (`run`).
//! - [`mod@turn`] — the turn-execution pipeline methods (`run_turn`,
//!   `run_cancellable_turn`, `handle_truncation_recovery`,
//!   `apply_turn_outcome`, `halt_turn_loop`) and the pure helpers
//!   (`fold_batched_tool_result`, `append_cancelled_tool_results_to_history`,
//!   `BatchStep`).
//!
//! The `pub(super)` surface — `spawn_rig_session` plus the six items `tests.rs`
//! imports — is re-exported from here so callers see the same paths as before
//! the split.

use std::thread;

use crossbeam_channel::unbounded;

use crate::{
    config::RigAgentConfig,
    contract::{
        Command, Event, Message as AgentMessage, MessageRole, ProviderEvent, SessionState,
        StartSession,
    },
    persistence::projection::duckdb::SharedDuckdbStore,
    registry::SessionHandle,
    roles::RoleDefinition,
    runtime_panic::catch_runtime_panic,
};

use super::completion::{
    complete_rig_turn, deterministic_rig_response, deterministic_tool_result_response,
};
use super::guards::{tool_result_fingerprint, GuardHalt, TurnLoopGuard};
use super::history::load_rig_session_history;
use super::mapping::rig_tool_result_message;
use super::model_limits::model_limits;
use super::session_prompt::{session_environment, session_extra_sections};
use super::{ClearingState, ToolCallDescriptor, TurnCompletion};

mod state;
mod turn;

// Re-export the public surface the parent `rig` module and tests depend on.
// These must keep resolving as `super::session::<name>` from `rig/mod.rs` and
// `tests.rs`.
#[allow(unused_imports)]
pub(crate) use state::SessionLoopState;
#[allow(unused_imports)]
pub(super) use turn::{
    append_cancelled_tool_results_to_history, fold_batched_tool_result, BatchStep,
};

pub(super) fn spawn_rig_session(
    request: StartSession,
    config: RigAgentConfig,
    role: Option<&'static RoleDefinition>,
    duckdb_cell: SharedDuckdbStore,
) -> SessionHandle {
    let (commands_tx, commands_rx) = unbounded();
    let (events_tx, events_rx) = unbounded::<ProviderEvent>();
    // Gathered once, right as the session starts, and reused for every
    // turn's system prompt — cwd/OS/git-repo status don't change over a
    // session's lifetime. Computed here (before `request` is partially
    // moved-from just below) from `request.workspace_root` -- the session's
    // own real root (an isolated worktree, post-isolation, when this
    // session is isolated), not this daemon process's own cwd -- so both
    // the prompt's "Working directory" line and `session_extra_sections`'s
    // skill discovery below reflect where this session actually runs.
    let environment = session_environment(&request);
    let provider_id = request.provider_id;
    let session_id = request.session_id;
    let fallback_events = request.history;
    let trusted_project = request.trusted_project;

    let panic_events_tx = events_tx.clone();
    thread::spawn(move || {
        let outcome = catch_runtime_panic(move || {
            // Blocks this dedicated thread (never the caller of `start_session`,
            // and never agentd's async accept loop) until the event-log
            // writer's own rebuild-or-open decision has landed -- see
            // `SharedDuckdbStore`'s doc comment for why this must be a genuine
            // wait, not "read whatever's there right now": reading too early
            // here (or through a fresh `Store::open`) is exactly the
            // resumed-session bug this fixed -- a session's own real history
            // silently not showing up.
            let duckdb_store = duckdb_cell.wait();
            let store_was_available = duckdb_store.is_some();
            let persisted =
                load_rig_session_history(duckdb_store.as_ref(), session_id, &fallback_events);
            let rig_history = persisted.messages;
            let cleared_call_ids = persisted.cleared_call_ids;
            // Issue 012: when the DuckDB projection store is unavailable and
            // the JSONL event log also yielded no reconstructable history for
            // a resumed session (events were present but produced no
            // messages), don't proceed silently — surface the failure as a
            // visible error in the frame so the operator knows the session
            // resumed without its provider history.
            if !store_was_available && !fallback_events.is_empty() && rig_history.is_empty() {
                let _ = events_tx.send(
                    Event::Error(crate::contract::Error {
                        message: "Provider history could not be loaded: the DuckDB \
                                  projection store is unavailable and the event log \
                                  yielded no reconstructable history."
                            .to_string(),
                    })
                    .into(),
                );
            }
            let extra_sections =
                session_extra_sections(&environment, &config, role, trusted_project);

            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                let _ = events_tx.send(
                    Event::Error(crate::contract::Error {
                        message: "Rig session unavailable: failed to create Tokio runtime."
                            .to_string(),
                    })
                    .into(),
                );
                let _ = events_tx.send(Event::StateChanged(SessionState::Terminated).into());
                return;
            };

            let _ = events_tx.send(Event::StateChanged(SessionState::Created).into());
            let _ = events_tx.send(
                Event::MessageCommitted(AgentMessage {
                    role: MessageRole::Assistant,
                    text: super::rig_initialization_message(
                        &provider_id,
                        &config,
                        rig_history.len(),
                    ),
                })
                .into(),
            );
            let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());

            runtime.block_on(async {
                let mut state = state::SessionLoopState::new(
                    session_id,
                    commands_rx,
                    events_tx,
                    config,
                    environment,
                    extra_sections,
                    role,
                    rig_history,
                    cleared_call_ids,
                )
                .await;
                state.run().await;
            });
        });
        if let Err(report) = outcome {
            let _ = panic_events_tx.send(
                Event::Error(crate::contract::Error {
                    message: report.message("internal Rig provider panic"),
                })
                .into(),
            );
        }
    });

    SessionHandle::new(commands_tx, events_rx)
}

/// Builds this session's [`ClearingState`] from the provider's declared
/// model limits.
///
/// The deterministic fallback provider never issues a provider request at
/// all, so it gets a disabled state without asking anyone. Otherwise the
/// effective window is `context_length − max_output_tokens`
/// (`docs/agent-compaction-design.md`); when the provider declares no
/// limits, clearing stays off for the session's whole life and says so
/// **once**, on stderr -- the design's `/models`-unavailable behavior, with
/// no guessed fallback window (see `super::model_limits`' module doc).
async fn discover_clearing_state(config: &RigAgentConfig) -> ClearingState {
    if !config.openai_enabled {
        return ClearingState::disabled();
    }
    let window = model_limits(config.base_url.as_deref(), &config.model)
        .await
        .and_then(|limits| limits.effective_window_tokens(config.max_output_tokens));
    if window.is_none() {
        eprintln!(
            "horizon-agent: `{}` declares no context_length at /models; history clearing is \
             disabled for this session (docs/agent-compaction-design.md)",
            config.model
        );
    }
    ClearingState::new(window, config.clearing_threshold_pct)
}

/// Forwards commands from the crossbeam channel (the provider's public,
/// synchronous surface — unchanged for callers) onto a tokio channel, so the
/// async session loop below can `select!` between receiving a command and
/// progressing an in-flight turn. This is what makes `Command::Cancel`
/// readable mid-turn instead of sitting unread behind a blocking `recv`.
fn bridge_commands(
    commands_rx: crossbeam_channel::Receiver<Command>,
) -> tokio::sync::mpsc::UnboundedReceiver<Command> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    thread::spawn(move || {
        while let Ok(command) = commands_rx.recv() {
            if tx.send(command).is_err() {
                break;
            }
        }
    });
    rx
}
