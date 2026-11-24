//! The rig session: spawns the OS thread, loads history, and drives the
//! [`SessionLoopState`] turn loop. Split into:
//!
//! - [`mod@state`] — [`SessionLoopState`] and command coordination.
//! - [`mod@progress`] — retained tool batches and halted results.
//! - [`mod@input`] — input routing and lifecycle-control priority.
//! - [`mod@tool_results`] — result acceptance, batch completion, and halt continuation.
//! - [`mod@interaction`] — fresh user/background interaction preparation.
//! - [`mod@turn`] — the turn-execution pipeline methods (`run_turn`,
//!   `run_cancellable_turn`, `handle_truncation_recovery`,
//!   `apply_turn_outcome`, `halt_turn_loop`).
//! - [`mod@settlement`] — host acknowledgement of stopped tool batches.
//! - [`mod@continuation`] — the turn-continuation check.
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
    registry::SessionHandle,
    roles::RoleDefinition,
    runtime_panic::catch_runtime_panic,
};

use super::completion::{
    complete_rig_turn, deterministic_rig_response, deterministic_tool_result_response,
};
use super::guards::TurnLoopGuard;
use super::history::load_rig_session_history;
use super::model_limits::model_limits;
use super::session_prompt::{session_environment, session_extra_sections};
use super::{ClearingState, ToolCallDescriptor, TurnCompletion};

mod continuation;
mod environment;
mod input;
mod interaction;
mod progress;
mod settlement;
mod state;
mod tool_results;
mod turn;

// Re-export the public surface the parent `rig` module and tests depend on.
// These must keep resolving as `super::session::<name>` from `rig/mod.rs` and
// `tests.rs`.
pub(crate) use state::SessionLoopState;

/// Spawns one rig session with its initial provider configuration.
pub(super) fn spawn_rig_session(
    request: StartSession,
    config: RigAgentConfig,
    role: Option<&'static RoleDefinition>,
) -> SessionHandle {
    let (commands_tx, commands_rx) = unbounded();
    let (events_tx, events_rx) = unbounded::<ProviderEvent>();
    // Gathered once, right as the session starts, and reused for every
    // turn's system prompt until an explicit environment activation. Computed here (before `request` is partially
    // moved-from just below) from `request.workspace_root` -- the session's
    // own real root (an isolated worktree, post-isolation, when this
    // session is isolated), not this daemon process's own cwd -- so both
    // the prompt's "Working directory at session start" line and
    // `session_extra_sections`'s
    // skill discovery below reflect where this session actually runs.
    let environment = session_environment(&request);
    let provider_id = request.provider_id;
    let session_id = request.session_id;
    let fallback_events = request.history;
    let restored_inputs = input::Inputs::restore(&fallback_events);
    let trusted_project = request.trusted_project;
    let composed = request.modules;
    // Thread `trusted_project` into the per-session config so
    // `rig_tool_definitions` can filter the knowledge tools out of the
    // advertised catalog for untrusted sessions. `role_adjusted_config`
    // (the caller) doesn't set this — trust is per-session, not
    // role-level.
    let mut config = config;
    config.trusted_project = trusted_project;

    let panic_events_tx = events_tx.clone();
    thread::spawn(move || {
        let outcome = catch_runtime_panic(move || {
            let persisted = match load_rig_session_history(&fallback_events) {
                Ok(history) => history,
                Err(message) => {
                    let _ = events_tx.send(Event::Error(crate::contract::Error { message }).into());
                    let _ = events_tx
                        .send(Event::StateChanged(crate::contract::SessionState::Failed).into());
                    return;
                }
            };
            let rig_history = persisted.messages;
            let cleared_occurrence_ids = persisted.cleared_occurrence_ids;
            let modules = match composed {
                Some(modules) => modules.as_slice().to_vec(),
                None => crate::modules::builtin(&config, role),
            };
            let extra_sections =
                session_extra_sections(&modules, &environment, &config, role, trusted_project);

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
            let _ = events_tx.send(crate::contract::ProviderEvent::SessionReasoning(
                super::reasoning_selection(&config),
            ));

            runtime.block_on(async {
                let mut state = state::SessionLoopState::new(
                    session_id,
                    commands_rx,
                    events_tx,
                    config,
                    environment,
                    extra_sections,
                    modules,
                    role,
                    rig_history,
                    cleared_occurrence_ids,
                )
                .await;
                state.inputs = restored_inputs;
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
/// limits, clearing stays off and says so on stderr -- the design's
/// `/models`-unavailable behavior, with no guessed fallback window (see
/// `super::model_limits`' module doc).
async fn discover_clearing_state(config: &mut RigAgentConfig) -> ClearingState {
    ClearingState::new(
        discover_effective_window(config).await,
        config.clearing_threshold_pct,
    )
}

/// The effective window for `config`'s current `(kind, base_url,
/// api_key_env, model)`. Shared by session start and
/// [`state::SessionLoopState::rediscover_clearing_window`], which runs after
/// a model change: the window belongs to the model, so a session that
/// switches models keeps clearing against the model it is now talking to.
/// `model_limits` caches per key, so a re-discovery of an already-seen
/// combination costs no request.
///
/// Also takes the model's declared maximum output into `config`
/// ([`RigAgentConfig::output_cap`]), which the window is reckoned with.
async fn discover_effective_window(config: &mut RigAgentConfig) -> Option<u64> {
    if !config.model_in_play() {
        return None;
    }
    let limits = model_limits(config).await;
    config.model_output_tokens = limits.and_then(|limits| limits.max_output_tokens);
    let window = limits.and_then(|limits| limits.effective_window_tokens(config.output_cap()));
    if window.is_none() {
        eprintln!(
            "horizon-agent: `{}` declares no context_length at /models and the model catalog \
             has none for it; history clearing is disabled for this session \
             (docs/agent-compaction-design.md)",
            config.model
        );
    }
    window
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
