use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    thread,
};

use crossbeam_channel::{unbounded, Sender};
use rig_core::completion::Message;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

use crate::{
    agent::config::RigAgentConfig,
    agent::contract::{
        Command, Error, Event, Message as AgentMessage, MessageRole, ProviderEvent, SessionHandle,
        SessionState, StartSession, ToolCallId, ToolCallResult,
    },
    agent::prompt::SessionEnvironment,
    agent::tools::cancelled_tool_call_result,
};

use super::{
    complete_rig_turn, deterministic_rig_response, deterministic_tool_result_response,
    load_rig_history, rig_initialization_message, rig_tool_result_message, ToolCallDescriptor,
    TurnCompletion,
};

pub(super) fn spawn_rig_session(
    request: StartSession,
    config: RigAgentConfig,
    memory_duckdb_path: Option<PathBuf>,
) -> SessionHandle {
    let (commands_tx, commands_rx) = unbounded();
    let (events_tx, events_rx) = unbounded::<ProviderEvent>();
    let provider_id = request.provider_id;
    let session_id = request.session_id;

    thread::spawn(move || {
        let rig_history = load_rig_history(memory_duckdb_path.as_deref(), session_id);
        // Gathered once, right as the session starts, and reused for every
        // turn's system prompt — cwd/OS/git-repo status don't change over a
        // session's lifetime.
        let environment = SessionEnvironment::current();

        let Ok(runtime) = tokio::runtime::Runtime::new() else {
            let _ = events_tx.send(
                Event::Error(Error {
                    message: "Rig session unavailable: failed to create Tokio runtime.".to_string(),
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
                text: rig_initialization_message(&provider_id, &config, rig_history.len()),
            })
            .into(),
        );
        let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());

        runtime.block_on(run_session_loop(
            commands_rx,
            events_tx,
            config,
            environment,
            rig_history,
        ));
    });

    SessionHandle::new(commands_tx, events_rx)
}

/// Forwards commands from the crossbeam channel (the provider's public,
/// synchronous surface — unchanged for callers) onto a tokio channel, so the
/// async session loop below can `select!` between receiving a command and
/// progressing an in-flight turn. This is what makes `Command::Cancel`
/// readable mid-turn instead of sitting unread behind a blocking `recv`.
fn bridge_commands(
    commands_rx: crossbeam_channel::Receiver<Command>,
) -> UnboundedReceiver<Command> {
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

async fn run_session_loop(
    commands_rx: crossbeam_channel::Receiver<Command>,
    events_tx: Sender<ProviderEvent>,
    config: RigAgentConfig,
    environment: SessionEnvironment,
    mut rig_history: Vec<Message>,
) {
    let mut commands = bridge_commands(commands_rx);
    let mut inbox: VecDeque<Command> = VecDeque::new();
    // Every tool call whose result is still outstanding, with the
    // descriptor (tool id + args) needed to fingerprint the eventual
    // result as (tool, args, output) for doom-loop detection.
    let mut pending_tool_calls: HashMap<ToolCallId, ToolCallDescriptor> = HashMap::new();
    let mut cancelled_call_ids: HashSet<ToolCallId> = HashSet::new();
    let mut guard = TurnLoopGuard::new();

    loop {
        let command = match inbox.pop_front() {
            Some(command) => command,
            None => match commands.recv().await {
                Some(command) => command,
                None => break,
            },
        };

        match command {
            Command::Initialize(_) => {
                let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
                let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
            }
            Command::UserMessage { text } => {
                // A fresh user message starts a new interaction: both loop
                // guards below count/track only *tool-driven* turns since
                // the last user message.
                guard.reset();
                let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
                let _ = events_tx.send(
                    Event::MessageCommitted(AgentMessage {
                        role: MessageRole::User,
                        text: text.clone(),
                    })
                    .into(),
                );
                let outcome = run_cancellable_turn(
                    &mut commands,
                    &mut inbox,
                    &config,
                    &environment,
                    &mut rig_history,
                    Message::user(text.clone()),
                    &events_tx,
                    || deterministic_rig_response(&text),
                )
                .await;
                apply_turn_outcome(
                    outcome,
                    &events_tx,
                    &mut rig_history,
                    &mut pending_tool_calls,
                    &mut cancelled_call_ids,
                );
            }
            Command::ToolCallResult(result) => {
                if cancelled_call_ids.remove(&result.call_id) {
                    // A result arriving after its turn was cancelled is
                    // accepted and silently dropped, per contract.
                    continue;
                }
                let Some(descriptor) = pending_tool_calls.remove(&result.call_id) else {
                    // Unsolicited (duplicate or stale) result: no pending
                    // tool call under this id. Running a turn from it would
                    // append an orphan tool-result message to rig_history —
                    // the next OpenAI request rejects a tool result with no
                    // matching assistant tool call — and stray results must
                    // not advance the loop guards. Accepted and silently
                    // dropped.
                    continue;
                };

                let fingerprint =
                    tool_result_fingerprint(&descriptor.tool_id, &descriptor.args, &result.output);
                let halt = guard
                    .record_tool_turn()
                    .or_else(|| guard.record_fingerprint(fingerprint));
                if let Some(halt) = halt {
                    // Stop instead of running another turn. The arrived
                    // result is real — its tool already executed — so it is
                    // recorded as-is; only *other* still-pending calls get
                    // the cancelled treatment.
                    halt_turn_loop(
                        halt,
                        &mut guard,
                        &events_tx,
                        &mut rig_history,
                        &result,
                        &mut pending_tool_calls,
                        &mut cancelled_call_ids,
                    );
                    continue;
                }

                let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
                let outcome = run_cancellable_turn(
                    &mut commands,
                    &mut inbox,
                    &config,
                    &environment,
                    &mut rig_history,
                    rig_tool_result_message(&result),
                    &events_tx,
                    || deterministic_tool_result_response(&result),
                )
                .await;
                apply_turn_outcome(
                    outcome,
                    &events_tx,
                    &mut rig_history,
                    &mut pending_tool_calls,
                    &mut cancelled_call_ids,
                );
            }
            Command::Cancel { .. } => {
                if pending_tool_calls.is_empty() {
                    // Nothing in flight (no running turn, no pending tool
                    // call) — cancel is a no-op in v1's "cancel whatever is
                    // in flight" semantics.
                    continue;
                }
                let call_ids: Vec<ToolCallId> =
                    pending_tool_calls.drain().map(|(id, _)| id).collect();
                cancelled_call_ids.extend(call_ids.iter().cloned());
                append_cancelled_tool_results_to_history(&mut rig_history, &call_ids);
                for call_id in call_ids {
                    let _ = events_tx
                        .send(Event::ToolCallFinished(cancelled_tool_call_result(call_id)).into());
                }
                let _ = events_tx.send(Event::StateChanged(SessionState::Cancelled).into());
                let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
            }
            Command::Shutdown => {
                let _ = events_tx.send(Event::StateChanged(SessionState::Terminated).into());
                break;
            }
            Command::ApproveToolCall { .. } | Command::DenyToolCall { .. } => {}
        }
    }
}

/// Runs a single rig turn to completion while concurrently listening for
/// `Command::Cancel`, so cancellation is readable mid-turn instead of
/// sitting behind the turn's blocking network call. Any other command
/// observed while the turn is in flight is queued in `inbox` and replayed by
/// the outer loop right after (in arrival order), so e.g. a `Shutdown` sent
/// mid-turn is never silently swallowed.
#[allow(clippy::too_many_arguments)]
async fn run_cancellable_turn(
    commands: &mut UnboundedReceiver<Command>,
    inbox: &mut VecDeque<Command>,
    config: &RigAgentConfig,
    environment: &SessionEnvironment,
    rig_history: &mut Vec<Message>,
    prompt: Message,
    events_tx: &Sender<ProviderEvent>,
    fallback: impl FnOnce() -> Message,
) -> TurnCompletion {
    let token = CancellationToken::new();
    let turn = complete_rig_turn(
        config,
        environment,
        rig_history,
        prompt,
        events_tx,
        fallback,
        &token,
    );
    tokio::pin!(turn);

    loop {
        tokio::select! {
            outcome = &mut turn => return outcome,
            maybe_command = commands.recv() => {
                match maybe_command {
                    Some(Command::Cancel { .. }) => token.cancel(),
                    Some(other) => inbox.push_back(other),
                    None => return turn.await,
                }
            }
        }
    }
}

fn apply_turn_outcome(
    outcome: TurnCompletion,
    events_tx: &Sender<ProviderEvent>,
    rig_history: &mut Vec<Message>,
    pending_tool_calls: &mut HashMap<ToolCallId, ToolCallDescriptor>,
    cancelled_call_ids: &mut HashSet<ToolCallId>,
) {
    if outcome.cancelled {
        cancelled_call_ids.extend(outcome.requested_tool_call_ids.iter().cloned());
        append_cancelled_tool_results_to_history(rig_history, &outcome.requested_tool_call_ids);
        for call_id in outcome.requested_tool_call_ids {
            let _ =
                events_tx.send(Event::ToolCallFinished(cancelled_tool_call_result(call_id)).into());
        }
        let _ = events_tx.send(Event::StateChanged(SessionState::Cancelled).into());
        let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
        return;
    }

    if outcome.requested_tool_call_ids.is_empty() {
        let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
    } else {
        pending_tool_calls.extend(outcome.requested_tool_calls);
    }
}

/// Appends one cancelled tool-result message per cancelled call id, directly
/// after the assistant message that carried the tool calls. This keeps the
/// rig history self-consistent for the API: an assistant `tool_calls`
/// message not followed by a result message per call is rejected by OpenAI
/// on the next request. Mirrors the cancelled `ToolCallFinished` events
/// synthesized for the UI and persistence.
pub(super) fn append_cancelled_tool_results_to_history(
    rig_history: &mut Vec<Message>,
    cancelled_call_ids: &[ToolCallId],
) {
    for call_id in cancelled_call_ids {
        rig_history.push(rig_tool_result_message(&cancelled_tool_call_result(
            call_id.clone(),
        )));
    }
}

// --- Turn-loop guards ------------------------------------------------------
//
// Two independent safety nets against a runaway tool-calling loop, per
// `docs/agent-tools-design.md`'s "Error Model and Loop Guards" section:
//
// - an iteration cap on consecutive tool-driven turns since the last user
//   message (a model that never stops calling tools), and
// - doom-loop detection on repeated identical (tool, args, result)
//   fingerprints (a model stuck re-issuing the same call to the same
//   effect).
//
// Both halt the same way: an explanatory `Error` event, the same
// cancellation machinery `Command::Cancel` uses for still-pending calls (so
// `rig_history` stays API-valid), and a return to `WaitingForUser` so the
// next user message works normally. `TurnLoopGuard` itself is pure (no
// I/O), so its counting and fingerprinting logic is unit-tested directly in
// `tests.rs`.

/// Consecutive-tool-turn iteration cap. A "tool-driven turn" is one
/// triggered by `Command::ToolCallResult`, as opposed to a fresh
/// `Command::UserMessage`.
pub(super) const TOOL_TURN_ITERATION_CAP: u32 = 25;

/// Doom-loop detection window: this many consecutive identical
/// (tool, args, output) fingerprints trip the guard.
pub(super) const DOOM_LOOP_WINDOW: usize = 3;

/// Why the turn loop halted itself rather than running another turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GuardHalt {
    IterationCapExceeded,
    DoomLoopDetected,
}

impl GuardHalt {
    fn message(self) -> String {
        match self {
            GuardHalt::IterationCapExceeded => format!(
                "Stopped after {TOOL_TURN_ITERATION_CAP} consecutive tool-driven turns without a \
                 new user message. The agent may be stuck in a loop — send a new message to \
                 continue."
            ),
            GuardHalt::DoomLoopDetected => format!(
                "Stopped after {DOOM_LOOP_WINDOW} consecutive identical tool results. The agent \
                 appears to be repeating the same tool call without making progress — send a new \
                 message to continue."
            ),
        }
    }
}

/// Pure turn-loop guard state: counts consecutive tool-driven turns since
/// the last user message, and keeps a short window of tool-result
/// fingerprints to detect a doom loop. Free of I/O so it can be tested
/// directly as a small unit, independent of the session's channels and
/// async plumbing.
#[derive(Debug, Default)]
pub(super) struct TurnLoopGuard {
    consecutive_tool_turns: u32,
    recent_fingerprints: VecDeque<u64>,
}

impl TurnLoopGuard {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Resets both the iteration count and the fingerprint window. Called
    /// when a `Command::UserMessage` starts a fresh interaction.
    pub(super) fn reset(&mut self) {
        self.consecutive_tool_turns = 0;
        self.recent_fingerprints.clear();
    }

    /// Records that a tool-driven turn is about to run. Returns
    /// `Some(GuardHalt::IterationCapExceeded)` once the cap is exceeded
    /// (i.e. on the `TOOL_TURN_ITERATION_CAP + 1`-th consecutive call).
    pub(super) fn record_tool_turn(&mut self) -> Option<GuardHalt> {
        self.consecutive_tool_turns += 1;
        (self.consecutive_tool_turns > TOOL_TURN_ITERATION_CAP)
            .then_some(GuardHalt::IterationCapExceeded)
    }

    /// Records an incoming tool result's fingerprint. Returns
    /// `Some(GuardHalt::DoomLoopDetected)` once the last `DOOM_LOOP_WINDOW`
    /// fingerprints are all identical.
    pub(super) fn record_fingerprint(&mut self, fingerprint: u64) -> Option<GuardHalt> {
        self.recent_fingerprints.push_back(fingerprint);
        if self.recent_fingerprints.len() > DOOM_LOOP_WINDOW {
            self.recent_fingerprints.pop_front();
        }
        let is_doom_loop = self.recent_fingerprints.len() == DOOM_LOOP_WINDOW
            && self.recent_fingerprints.iter().all(|fp| *fp == fingerprint);
        is_doom_loop.then_some(GuardHalt::DoomLoopDetected)
    }
}

/// Fingerprints a tool result as (tool, args, output) — the triple the
/// design doc specifies. Args are included so distinct, productive calls
/// that happen to return identical output (e.g. greps for different
/// patterns, each with zero matches) are not mistaken for a doom loop.
/// Call ids are deliberately excluded: each call gets a fresh id even when
/// the model repeats the same call verbatim.
pub(super) fn tool_result_fingerprint(
    tool_id: &str,
    args: &serde_json::Value,
    output: &serde_json::Value,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tool_id.hash(&mut hasher);
    args.to_string().hash(&mut hasher);
    output.to_string().hash(&mut hasher);
    hasher.finish()
}

/// Halts the turn loop in response to a tripped guard.
///
/// The result that tripped the guard (`arrived_result`) is *real*: its tool
/// already executed (an `fs.write` is already on disk) and the app already
/// surfaced its genuine `ToolCallFinished`. So it is recorded in
/// `rig_history` as its actual output — never falsified as cancelled, and
/// with no second, contradictory `ToolCallFinished` event. Only the turn
/// that would have consumed it is skipped. Any *other* still-pending calls
/// are cancelled with the same helpers `Command::Cancel` uses (so
/// `rig_history` stays API-valid — see
/// `append_cancelled_tool_results_to_history`). Emits an explanatory
/// `Error` event, resets the guard, and returns the session to
/// `WaitingForUser` so the next `Command::UserMessage` works normally.
///
/// The caller must have already removed `arrived_result`'s call id from
/// `pending_tool_calls` (the session loop does this when it looks up the
/// call's descriptor).
pub(super) fn halt_turn_loop(
    halt: GuardHalt,
    guard: &mut TurnLoopGuard,
    events_tx: &Sender<ProviderEvent>,
    rig_history: &mut Vec<Message>,
    arrived_result: &ToolCallResult,
    pending_tool_calls: &mut HashMap<ToolCallId, ToolCallDescriptor>,
    cancelled_call_ids: &mut HashSet<ToolCallId>,
) {
    rig_history.push(rig_tool_result_message(arrived_result));

    let _ = events_tx.send(
        Event::Error(Error {
            message: halt.message(),
        })
        .into(),
    );

    let call_ids: Vec<ToolCallId> = pending_tool_calls.drain().map(|(id, _)| id).collect();
    cancelled_call_ids.extend(call_ids.iter().cloned());
    append_cancelled_tool_results_to_history(rig_history, &call_ids);
    for call_id in call_ids {
        let _ = events_tx.send(Event::ToolCallFinished(cancelled_tool_call_result(call_id)).into());
    }

    guard.reset();
    let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
}
