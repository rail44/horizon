use std::{
    collections::{HashMap, HashSet, VecDeque},
    thread,
};

use crossbeam_channel::{unbounded, Sender};
use rig_core::completion::Message;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

use crate::{
    config::RigAgentConfig,
    contract::{
        Command, Error, Event, Message as AgentMessage, MessageRole, ProviderEvent, SessionState,
        StartSession, ToolCallId, ToolCallResult, TurnEndReason,
    },
    persistence::projection::duckdb::SharedDuckdbStore,
    prompt::SessionEnvironment,
    registry::SessionHandle,
    roles::RoleDefinition,
    runtime_panic::catch_runtime_panic,
    tools::cancelled_tool_call_result,
};

use super::guards::{tool_result_fingerprint, GuardHalt, TurnLoopGuard};
use super::session_prompt::{session_environment, session_extra_sections};
use super::{
    complete_rig_turn, deterministic_rig_response, deterministic_tool_result_response,
    load_rig_session_history, model_limits::model_limits, rig_initialization_message,
    rig_tool_result_message, ClearingState, ToolCallDescriptor, TurnCompletion,
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
                    Event::Error(Error {
                        message: "Provider history could not be loaded: the DuckDB \
                                  projection store is unavailable and the event log \
                                  yielded no reconstructable history."
                            .to_string(),
                    })
                    .into(),
                );
            }
            let extra_sections = session_extra_sections(&environment, &config, role);

            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                let _ = events_tx.send(
                    Event::Error(Error {
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
                    text: rig_initialization_message(&provider_id, &config, rig_history.len()),
                })
                .into(),
            );
            let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());

            runtime.block_on(run_session_loop(
                session_id,
                commands_rx,
                events_tx,
                config,
                environment,
                extra_sections,
                role,
                rig_history,
                cleared_call_ids,
            ));
        });
        if let Err(report) = outcome {
            let _ = panic_events_tx.send(
                Event::Error(Error {
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

/// What the session loop woke up for: an inbound command, or a background
/// `task` child finishing while no provider round was pending
/// (`docs/agent-async-task-design.md` decision 2's auto-turn wake). Kept as
/// a value the `select!` *returns* rather than work done inside a handler,
/// because the wake's handling needs `&mut` access to state the command
/// future itself borrows.
enum Next {
    Command(Command),
    TaskWake,
    Closed,
}

#[allow(clippy::too_many_arguments)]
async fn run_session_loop(
    session_id: crate::contract::SessionId,
    commands_rx: crossbeam_channel::Receiver<Command>,
    events_tx: Sender<ProviderEvent>,
    config: RigAgentConfig,
    environment: SessionEnvironment,
    extra_sections: Vec<String>,
    role: Option<&'static RoleDefinition>,
    mut rig_history: Vec<Message>,
    cleared_call_ids: Vec<ToolCallId>,
) {
    // Tier 1 compaction state (`docs/agent-compaction-design.md`,
    // `super::clearing`): the discovered window plus whatever clearing
    // passes this session already froze before it was resumed. Resolved
    // here, inside the loop's own runtime, rather than before the session's
    // opening events -- the `/models` lookup is cached per process, but the
    // first session in a process must not have its "session started"
    // feedback held behind a network round trip.
    let mut clearing = discover_clearing_state(&config).await;
    clearing.seed_cleared(cleared_call_ids);
    let mut commands = bridge_commands(commands_rx);
    // The completion-subscription seam's receiving end for this session
    // (`tools::explore`): signalled whenever one of this session's
    // background `task` children finishes. Registered before the first
    // command is read so a child launched in the very first turn can never
    // finish into a session with no wake channel.
    let mut task_wake = crate::tools::register_wake(session_id);
    let mut inbox: VecDeque<Command> = VecDeque::new();
    // Every tool call whose result is still outstanding, with the
    // descriptor (tool id + args) needed to fingerprint the eventual
    // result as (tool, args, output) for doom-loop detection.
    let mut pending_tool_calls: HashMap<ToolCallId, ToolCallDescriptor> = HashMap::new();
    let mut cancelled_call_ids: HashSet<ToolCallId> = HashSet::new();
    let mut guard = TurnLoopGuard::new(config.iteration_cap, config.doom_loop_window);
    // The real, already-executed tool result a guard halt stashed instead
    // of folding into `rig_history` right away -- see `halt_turn_loop`'s
    // doc comment. `Command::ContinueTurn` consumes it to resume the turn
    // loop; `Command::UserMessage` flushes it into history first if the
    // user types past the halt instead of clicking Continue. `None`
    // whenever the session isn't sitting on a halted turn, including right
    // after a fresh start (a replayed session must never auto-resume).
    let mut pending_halt_result: Option<ToolCallResult> = None;

    loop {
        let next = match inbox.pop_front() {
            Some(command) => Next::Command(command),
            None => tokio::select! {
                maybe_command = commands.recv() => match maybe_command {
                    Some(command) => Next::Command(command),
                    None => Next::Closed,
                },
                Some(()) = task_wake.recv() => Next::TaskWake,
            },
        };

        let command = match next {
            Next::Closed => break,
            Next::Command(command) => command,
            Next::TaskWake => {
                // A background `task` child finished. If a tool batch is
                // still outstanding -- which includes a call parked on an
                // approval -- a provider round is still coming, and the
                // drain that runs before it will carry the notification
                // instead; nothing to do here. Otherwise the turn has
                // already ended, so the notification becomes a new turn's
                // synthetic input. That turn is an ordinary one:
                // `Event::TurnEnded` remains the only turn boundary
                // external monitors need to trust.
                if !pending_tool_calls.is_empty() {
                    continue;
                }
                let Some(text) = crate::tools::take_notification(session_id) else {
                    continue;
                };
                // The same flush `Command::UserMessage` performs: a result
                // a guard halt stashed still has to land in `rig_history`
                // before the next request, or the API rejects an assistant
                // `tool_calls` message with no matching result.
                if let Some(result) = pending_halt_result.take() {
                    rig_history.push(rig_tool_result_message(&result));
                }
                guard.reset();
                let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
                let _ = events_tx.send(crate::tools::notification_event(text.clone()).into());
                let fallback_text = text.clone();
                let outcome = run_cancellable_turn(
                    &mut commands,
                    &mut inbox,
                    &config,
                    &environment,
                    &extra_sections,
                    &mut rig_history,
                    Message::user(text),
                    &events_tx,
                    &mut clearing,
                    move || deterministic_rig_response(&fallback_text),
                )
                .await;
                if let Some(outcome) = handle_truncation_recovery(
                    outcome,
                    &mut commands,
                    &mut inbox,
                    &config,
                    &environment,
                    &extra_sections,
                    &mut rig_history,
                    &events_tx,
                    &mut clearing,
                    &mut guard,
                    &mut pending_tool_calls,
                    &mut cancelled_call_ids,
                )
                .await
                {
                    apply_turn_outcome(
                        outcome,
                        &events_tx,
                        &mut rig_history,
                        &mut pending_tool_calls,
                        &mut cancelled_call_ids,
                    );
                }
                continue;
            }
        };

        match command {
            Command::Initialize(_) => {
                let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
                let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
            }
            // Skew catch-all (`Command::Unknown`'s doc): a command this
            // build can't name is logged and dropped -- never acked, never
            // half-executed.
            Command::Unknown => {
                tracing::warn!("ignoring unknown agent command from a newer peer");
            }
            Command::UserMessage { text } => {
                // A user message starts a new interaction rather than joining
                // the previous turn's tool batch. This command can arrive
                // while any kind of tool is still running or awaiting
                // approval, so retire the whole old batch before asking the
                // provider to handle the new message. Otherwise those old
                // call ids remain in `pending_tool_calls` and a result from
                // the new turn is mistaken for a non-final member of the old
                // batch, leaving the session waiting forever.
                if cancel_outstanding_tool_calls(
                    &events_tx,
                    &mut rig_history,
                    &mut pending_tool_calls,
                    &mut cancelled_call_ids,
                ) {
                    emit_cancelled_turn(&events_tx);
                }
                // Typing past a halt instead of clicking Continue: the
                // real result a guard halt stashed still has to land in
                // `rig_history` before the next request, or the API
                // rejects it (an assistant `tool_calls` message with no
                // matching result). A no-op when there's nothing pending.
                if let Some(result) = pending_halt_result.take() {
                    rig_history.push(rig_tool_result_message(&result));
                }
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
                let (prompt, injected) = inject_task_notification(
                    session_id,
                    &events_tx,
                    &mut rig_history,
                    Message::user(text.clone()),
                );
                let fallback_text = injected.unwrap_or(text);
                let outcome = run_cancellable_turn(
                    &mut commands,
                    &mut inbox,
                    &config,
                    &environment,
                    &extra_sections,
                    &mut rig_history,
                    prompt,
                    &events_tx,
                    &mut clearing,
                    move || deterministic_rig_response(&fallback_text),
                )
                .await;
                if let Some(outcome) = handle_truncation_recovery(
                    outcome,
                    &mut commands,
                    &mut inbox,
                    &config,
                    &environment,
                    &extra_sections,
                    &mut rig_history,
                    &events_tx,
                    &mut clearing,
                    &mut guard,
                    &mut pending_tool_calls,
                    &mut cancelled_call_ids,
                )
                .await
                {
                    apply_turn_outcome(
                        outcome,
                        &events_tx,
                        &mut rig_history,
                        &mut pending_tool_calls,
                        &mut cancelled_call_ids,
                    );
                }
            }
            Command::ToolCallResult(result) => {
                if cancelled_call_ids.remove(&result.call_id) {
                    // A result arriving after its turn was cancelled is
                    // accepted and silently dropped, per contract. This
                    // also covers the rest of a cancelled batch: `Cancel`
                    // drains every still-outstanding call id into
                    // `cancelled_call_ids` (below), so each of their real
                    // results, arriving later, lands here and is dropped
                    // rather than starting a turn.
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

                // Doom-loop fingerprinting is per *result* (every call's
                // outcome must be checked, not just the batch's last), so
                // it runs unconditionally here — before deciding whether
                // this is the last outstanding result of the current batch.
                let fingerprint =
                    tool_result_fingerprint(&descriptor.tool_id, &descriptor.args, &result.output);
                if let Some(halt) = guard.record_fingerprint(fingerprint) {
                    // Stop instead of running another turn. The arrived
                    // result is real — its tool already executed — so it is
                    // recorded as-is; only *other* still-pending calls get
                    // the cancelled treatment.
                    halt_turn_loop(
                        halt,
                        &mut guard,
                        &mut commands,
                        &mut inbox,
                        &config,
                        &environment,
                        &extra_sections,
                        role,
                        &events_tx,
                        &mut pending_halt_result,
                        &mut rig_history,
                        &mut clearing,
                        &result,
                        &mut pending_tool_calls,
                        &mut cancelled_call_ids,
                    )
                    .await;
                    continue;
                }

                if fold_batched_tool_result(&mut rig_history, &pending_tool_calls, &result)
                    == BatchStep::Continue
                {
                    continue;
                }

                // The whole batch has landed: this is the one tool-driven
                // turn the batch counts as, so the iteration-cap guard is
                // recorded exactly once here — never per result, or an
                // N-call batch would burn the cap N times faster.
                if let Some(halt) = guard.record_tool_turn() {
                    halt_turn_loop(
                        halt,
                        &mut guard,
                        &mut commands,
                        &mut inbox,
                        &config,
                        &environment,
                        &extra_sections,
                        role,
                        &events_tx,
                        &mut pending_halt_result,
                        &mut rig_history,
                        &mut clearing,
                        &result,
                        &mut pending_tool_calls,
                        &mut cancelled_call_ids,
                    )
                    .await;
                    continue;
                }

                let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
                let (prompt, injected) = inject_task_notification(
                    session_id,
                    &events_tx,
                    &mut rig_history,
                    rig_tool_result_message(&result),
                );
                let outcome = run_cancellable_turn(
                    &mut commands,
                    &mut inbox,
                    &config,
                    &environment,
                    &extra_sections,
                    &mut rig_history,
                    prompt,
                    &events_tx,
                    &mut clearing,
                    move || match injected {
                        Some(text) => deterministic_rig_response(&text),
                        None => deterministic_tool_result_response(&result),
                    },
                )
                .await;
                if let Some(outcome) = handle_truncation_recovery(
                    outcome,
                    &mut commands,
                    &mut inbox,
                    &config,
                    &environment,
                    &extra_sections,
                    &mut rig_history,
                    &events_tx,
                    &mut clearing,
                    &mut guard,
                    &mut pending_tool_calls,
                    &mut cancelled_call_ids,
                )
                .await
                {
                    apply_turn_outcome(
                        outcome,
                        &events_tx,
                        &mut rig_history,
                        &mut pending_tool_calls,
                        &mut cancelled_call_ids,
                    );
                }
            }
            Command::ContinueTurn => {
                let Some(result) = pending_halt_result.take() else {
                    // Nothing halted to resume: a safe no-op. Covers a
                    // stale Continue arriving after a fresh user message
                    // already flushed the pending result, a Continue sent
                    // to an idle/never-halted session, and — critically —
                    // a resumed session right after bootstrap: replay never
                    // populates `pending_halt_result` on its own, so a
                    // persisted session that ended halted stays halted
                    // (waiting-for-user) rather than auto-resuming.
                    continue;
                };
                guard.reset();
                // Counts as the resumed turn's one tool-driven turn, the
                // same as the `Command::ToolCallResult` arm above would
                // have — keeps the guard meaningful even if Continue is
                // clicked repeatedly on a genuinely runaway loop: it can
                // re-trip after another full `iteration_cap` turns rather
                // than being permanently defeated by one reset.
                if let Some(halt) = guard.record_tool_turn() {
                    halt_turn_loop(
                        halt,
                        &mut guard,
                        &mut commands,
                        &mut inbox,
                        &config,
                        &environment,
                        &extra_sections,
                        role,
                        &events_tx,
                        &mut pending_halt_result,
                        &mut rig_history,
                        &mut clearing,
                        &result,
                        &mut pending_tool_calls,
                        &mut cancelled_call_ids,
                    )
                    .await;
                    continue;
                }
                let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
                let (prompt, injected) = inject_task_notification(
                    session_id,
                    &events_tx,
                    &mut rig_history,
                    rig_tool_result_message(&result),
                );
                let outcome = run_cancellable_turn(
                    &mut commands,
                    &mut inbox,
                    &config,
                    &environment,
                    &extra_sections,
                    &mut rig_history,
                    prompt,
                    &events_tx,
                    &mut clearing,
                    move || match injected {
                        Some(text) => deterministic_rig_response(&text),
                        None => deterministic_tool_result_response(&result),
                    },
                )
                .await;
                if let Some(outcome) = handle_truncation_recovery(
                    outcome,
                    &mut commands,
                    &mut inbox,
                    &config,
                    &environment,
                    &extra_sections,
                    &mut rig_history,
                    &events_tx,
                    &mut clearing,
                    &mut guard,
                    &mut pending_tool_calls,
                    &mut cancelled_call_ids,
                )
                .await
                {
                    apply_turn_outcome(
                        outcome,
                        &events_tx,
                        &mut rig_history,
                        &mut pending_tool_calls,
                        &mut cancelled_call_ids,
                    );
                }
            }
            Command::Cancel { .. } => {
                if !cancel_outstanding_tool_calls(
                    &events_tx,
                    &mut rig_history,
                    &mut pending_tool_calls,
                    &mut cancelled_call_ids,
                ) {
                    // Nothing in flight (no running turn, no pending tool
                    // call) — cancel is a no-op in v1's "cancel whatever is
                    // in flight" semantics.
                    continue;
                }
                emit_cancelled_turn(&events_tx);
            }
            Command::Shutdown => {
                let _ = events_tx.send(Event::StateChanged(SessionState::Terminated).into());
                break;
            }
            Command::ApproveToolCall { .. } | Command::DenyToolCall { .. } => {}
        }
    }

    crate::tools::unregister_wake(session_id);
}

/// Drains this session's finished background `task` children and, if any
/// were waiting, turns the whole batch into the message this provider round
/// actually carries (`docs/agent-async-task-design.md` decision 2: "before
/// each provider round of the requester's turn loop, drain the queue").
///
/// The mechanics matter for history validity. `prompt` is whatever the
/// round would otherwise have sent -- a user message, or the tool result
/// that completed a batch. When a notification is waiting, that original
/// prompt is pushed into `rig_history` here and the notification becomes
/// the new prompt, so the request reads `assistant(tool_calls) →
/// tool_result(s) → user(notification) → assistant(reply)`: the tool
/// results still sit directly behind the calls they answer, and the
/// notification is an ordinary user-role text turn on top. Reversing the
/// two would separate a tool call from its result and be rejected.
///
/// Returns the prompt to send plus the notification text, which the caller
/// needs only to drive the deterministic fallback provider (no network
/// mode) off the message actually sent.
fn inject_task_notification(
    session_id: crate::contract::SessionId,
    events_tx: &Sender<ProviderEvent>,
    rig_history: &mut Vec<Message>,
    prompt: Message,
) -> (Message, Option<String>) {
    let Some(text) = crate::tools::take_notification(session_id) else {
        return (prompt, None);
    };
    rig_history.push(prompt);
    let _ = events_tx.send(crate::tools::notification_event(text.clone()).into());
    (Message::user(text.clone()), Some(text))
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
    extra_sections: &[String],
    rig_history: &mut Vec<Message>,
    prompt: Message,
    events_tx: &Sender<ProviderEvent>,
    clearing: &mut ClearingState,
    fallback: impl FnOnce() -> Message,
) -> TurnCompletion {
    let token = CancellationToken::new();
    let turn = complete_rig_turn(
        config,
        environment,
        extra_sections,
        rig_history,
        prompt,
        events_tx,
        clearing,
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

/// Centralizes `Event::TurnEnded` emission for every turn-completion path
/// that runs a rig turn (`run_cancellable_turn`/`complete_rig_turn`):
/// completed, cancelled, and failed all funnel through here (the two
/// guard-halted stop reasons come from the turn-loop guard's own
/// [`halt_turn_loop`], which never calls this — a halt stops the loop
/// *instead of* running another turn, so there's no `TurnCompletion` for it
/// to inspect). `outcome.failed` is checked before the empty-tool-calls
/// branch since a failed provider request also requests no tool calls —
/// without the explicit flag the two would be indistinguishable.
pub(super) fn apply_turn_outcome(
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
        let _ = events_tx.send(Event::TurnEnded(TurnEndReason::Cancelled).into());
        let _ = events_tx.send(Event::StateChanged(SessionState::Cancelled).into());
        let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
        return;
    }

    if outcome.failed {
        let _ = events_tx.send(Event::TurnEnded(TurnEndReason::Failed).into());
        let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
        return;
    }

    if outcome.requested_tool_call_ids.is_empty() {
        let _ = events_tx.send(Event::TurnEnded(TurnEndReason::Completed).into());
        let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
    } else {
        pending_tool_calls.extend(outcome.requested_tool_calls);
    }
}

/// The synthetic continuation prompt injected after the harness detects
/// the provider truncated tool calls mid-stream. The model is told its
/// previous response was cut short and asked to continue the work it
/// described in its reasoning.
fn truncation_continuation_prompt(truncated_count: usize) -> String {
    format!(
        "The provider cut short {truncated_count} tool call(s) in your previous \
         response — the call(s) started streaming but were never finalized. \
         Continue the work you described in your reasoning and re-issue the \
         tool call(s)."
    )
}

/// Handles a turn outcome that may be truncated: if the provider started
/// streaming tool calls but never finalized them (`outcome.truncated`), the
/// turn is closed as `Failed` and the harness automatically continues with a
/// synthetic prompt, up to [`MAX_CONSECUTIVE_TRUNCATION_CONTINUES`] times
/// before falling back to `WaitingForUser`.
///
/// Returns `None` when the truncation was fully handled (the caller should
/// do nothing further), or `Some(outcome)` when the turn was not truncated
/// (the caller should pass it to `apply_turn_outcome`).
#[allow(clippy::too_many_arguments)]
async fn handle_truncation_recovery(
    mut outcome: TurnCompletion,
    commands: &mut UnboundedReceiver<Command>,
    inbox: &mut VecDeque<Command>,
    config: &RigAgentConfig,
    environment: &SessionEnvironment,
    extra_sections: &[String],
    rig_history: &mut Vec<Message>,
    events_tx: &Sender<ProviderEvent>,
    clearing: &mut ClearingState,
    guard: &mut TurnLoopGuard,
    pending_tool_calls: &mut HashMap<ToolCallId, ToolCallDescriptor>,
    cancelled_call_ids: &mut HashSet<ToolCallId>,
) -> Option<TurnCompletion> {
    if !outcome.truncated {
        guard.reset_truncation_counter();
        return Some(outcome);
    }

    loop {
        let count = outcome.truncated_tool_call_count;

        // Cancel any finalized tool calls from the truncated turn — the
        // turn is ending as Failed, so they must not hang as pending.
        if !outcome.requested_tool_call_ids.is_empty() {
            cancelled_call_ids.extend(outcome.requested_tool_call_ids.iter().cloned());
            append_cancelled_tool_results_to_history(rig_history, &outcome.requested_tool_call_ids);
            for call_id in &outcome.requested_tool_call_ids {
                let _ = events_tx.send(
                    Event::ToolCallFinished(cancelled_tool_call_result(call_id.clone())).into(),
                );
            }
        }

        // The event log must tell the truth: this was not a normal
        // completion. The turn is Failed, not Completed.
        let _ = events_tx.send(
            Event::Error(Error {
                message: format!(
                    "Provider truncated {count} tool call(s) mid-stream — \
                     the call(s) started streaming but were never finalized."
                ),
            })
            .into(),
        );
        let _ = events_tx.send(Event::TurnEnded(TurnEndReason::Failed).into());

        if !guard.record_truncation_continue() {
            // Consecutive truncation cap exhausted: stop auto-continuing
            // and let the user take over.
            let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
            return None;
        }

        // Auto-continue: inject the synthetic continuation prompt and run
        // the next turn, mirroring the TaskWake auto-start seam.
        let text = truncation_continuation_prompt(count);
        let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
        let _ = events_tx.send(
            Event::MessageCommitted(AgentMessage {
                role: MessageRole::AutoContinue,
                text: text.clone(),
            })
            .into(),
        );
        outcome = run_cancellable_turn(
            commands,
            inbox,
            config,
            environment,
            extra_sections,
            rig_history,
            Message::user(text),
            events_tx,
            clearing,
            || deterministic_rig_response("truncation recovery"),
        )
        .await;

        if !outcome.truncated {
            guard.reset_truncation_counter();
            apply_turn_outcome(
                outcome,
                events_tx,
                rig_history,
                pending_tool_calls,
                cancelled_call_ids,
            );
            return None;
        }
    }
}

/// What the `Command::ToolCallResult` arm should do next for a landed batch
/// member, once [`fold_batched_tool_result`] has decided whether the rest of
/// the batch is still outstanding.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum BatchStep {
    /// More of the batch is still outstanding. The result has already been
    /// folded into `rig_history`, in arrival order — the caller just keeps
    /// consuming commands, without emitting `Running` or running a turn.
    Continue,
    /// The whole batch has landed (this was its last outstanding call), so
    /// a follow-up completion should run. The result is deliberately *not*
    /// yet in `rig_history` — the caller runs the turn with it as the
    /// prompt message, which appends it right before the resulting
    /// assistant message (`run_cancellable_turn`/`complete_rig_turn`),
    /// keeping a single unbroken "tool_calls, then all N results, then the
    /// assistant's reply" run in history.
    RunTurn,
}

/// Decides what a landed `Command::ToolCallResult` should do, per the
/// "batching" fix in `run_session_loop`'s `Command::ToolCallResult` arm: a
/// single completion can request several parallel tool calls (e.g. MiniMax
/// routinely requesting 4 parallel `fs.read`s), each of which arrives as its
/// own `Command::ToolCallResult`. Running a follow-up completion per result
/// would send the model a protocol-malformed history (an assistant
/// `tool_calls` message missing most of its results) for every
/// still-outstanding call, and burn the iteration-cap guard once per result
/// instead of once per batch.
///
/// The caller must have already removed `result`'s call id from
/// `pending_tool_calls` (to look up its descriptor for the doom-loop
/// fingerprint) before calling this — so an empty `pending_tool_calls` here
/// means `result` was the batch's last outstanding call.
pub(super) fn fold_batched_tool_result(
    rig_history: &mut Vec<Message>,
    pending_tool_calls: &HashMap<ToolCallId, ToolCallDescriptor>,
    result: &ToolCallResult,
) -> BatchStep {
    if pending_tool_calls.is_empty() {
        BatchStep::RunTurn
    } else {
        rig_history.push(rig_tool_result_message(result));
        BatchStep::Continue
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

/// Retires every tool call still owned by the unfinished turn.
///
/// This operates on normalized provider call ids, not tool implementations,
/// so the same path covers synchronous filesystem/config tools, asynchronous
/// bash/web tools, and approval-gated calls. Real results that arrive after
/// retirement are recognized through `cancelled_call_ids` and dropped by the
/// session loop instead of entering a later turn's batch.
fn cancel_outstanding_tool_calls(
    events_tx: &Sender<ProviderEvent>,
    rig_history: &mut Vec<Message>,
    pending_tool_calls: &mut HashMap<ToolCallId, ToolCallDescriptor>,
    cancelled_call_ids: &mut HashSet<ToolCallId>,
) -> bool {
    let call_ids: Vec<ToolCallId> = pending_tool_calls.drain().map(|(id, _)| id).collect();
    if call_ids.is_empty() {
        return false;
    }

    cancelled_call_ids.extend(call_ids.iter().cloned());
    append_cancelled_tool_results_to_history(rig_history, &call_ids);
    for call_id in call_ids {
        let _ = events_tx.send(Event::ToolCallFinished(cancelled_tool_call_result(call_id)).into());
    }
    true
}

fn emit_cancelled_turn(events_tx: &Sender<ProviderEvent>) {
    let _ = events_tx.send(Event::TurnEnded(TurnEndReason::Cancelled).into());
    let _ = events_tx.send(Event::StateChanged(SessionState::Cancelled).into());
    let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
}

// --- Turn-loop guard response ---------------------------------------------
//
// The pure guard *detectors* (iteration cap + doom-loop fingerprinting,
// `GuardHalt`/`TurnLoopGuard`/`tool_result_fingerprint`) live in `guards.rs`.
// What remains here is the guard's *response*: `halt_turn_loop` cancels
// still-pending calls, optionally runs a cap-summary turn, emits the calm
// `TurnEnded` reason, and returns the session to `WaitingForUser` — work
// coupled to the session's turn-execution machinery, so it stays here.

/// Halts the turn loop in response to a tripped guard.
///
/// `docs/issues/002-agent-iteration-cap-halts-real-work.md`'s resolution
/// (decision 2): a guard halt now reads as a pause, not an error, so this
/// no longer emits `Event::Error` at all — only `Event::TurnEnded` with the
/// specific guard-kind reason (folded by `frame::apply_agent_event_to_frame`
/// into the turn's receipt, rendered calmly by `src/agent/turns/receipt.rs`
/// rather than as a danger-styled error block).
///
/// The result that tripped the guard (`arrived_result`) is *real*: its tool
/// already executed (an `fs.write` is already on disk) and the app already
/// surfaced its genuine `ToolCallFinished`. Any *other* still-pending calls
/// in the batch (only possible on the doom-loop path — see the module doc)
/// are cancelled immediately with the same helpers `Command::Cancel` uses,
/// since those never get a second chance to land.
///
/// For an iteration-cap halt on a role that opts in
/// (`RoleDefinition::summarize_on_cap`, e.g. `EXPLORE_ROLE` -- see
/// `docs/agent-explore-design.md`'s 2026-07-27 addendum and
/// `docs/research/agent-context-reduction-prior-art-2026-07-26.md` §4's
/// OpenCode/Hermes precedent), [`run_cap_summary_turn`] first folds
/// `arrived_result` into `rig_history` and runs one forced, tools-disabled
/// completion asking the model to summarize instead of stopping cold. If
/// that succeeds, the turn ends right there with the summary already
/// committed as the session's final message for this turn. Every other
/// case (doom loop, a role that doesn't opt in, or the wrap-up completion
/// itself failing) falls back to the original behavior: `arrived_result` is
/// deliberately *not* folded into `rig_history` here — `pending_halt_result`
/// stashes it instead, the same way an ordinary tool-driven turn treats a
/// batch's last-landed result: as the *next* turn's prompt (see
/// [`fold_batched_tool_result`]'s doc comment), not a pre-pushed history
/// entry. `Command::ContinueTurn` consumes it to resume; `Command::
/// UserMessage` flushes it into history first if the user types past the
/// halt instead.
///
/// Resets the guard and returns the session to `WaitingForUser` either way
/// (Continue re-enters the loop with a fresh guard, exactly like a new
/// `Command::UserMessage` would).
///
/// The caller must have already removed `arrived_result`'s call id from
/// `pending_tool_calls` (the session loop does this when it looks up the
/// call's descriptor).
#[allow(clippy::too_many_arguments)]
pub(super) async fn halt_turn_loop(
    halt: GuardHalt,
    guard: &mut TurnLoopGuard,
    commands: &mut UnboundedReceiver<Command>,
    inbox: &mut VecDeque<Command>,
    config: &RigAgentConfig,
    environment: &SessionEnvironment,
    extra_sections: &[String],
    role: Option<&'static RoleDefinition>,
    events_tx: &Sender<ProviderEvent>,
    pending_halt_result: &mut Option<ToolCallResult>,
    rig_history: &mut Vec<Message>,
    clearing: &mut ClearingState,
    arrived_result: &ToolCallResult,
    pending_tool_calls: &mut HashMap<ToolCallId, ToolCallDescriptor>,
    cancelled_call_ids: &mut HashSet<ToolCallId>,
) {
    cancel_outstanding_tool_calls(
        events_tx,
        rig_history,
        pending_tool_calls,
        cancelled_call_ids,
    );

    let summarized = halt == GuardHalt::IterationCapExceeded
        && role.is_some_and(|role| role.summarize_on_cap)
        && run_cap_summary_turn(
            commands,
            inbox,
            config,
            environment,
            extra_sections,
            rig_history,
            clearing,
            arrived_result,
            events_tx,
        )
        .await;

    if !summarized {
        *pending_halt_result = Some(arrived_result.clone());
    }

    guard.reset();
    let _ = events_tx.send(Event::TurnEnded(halt.turn_end_reason()).into());
    let _ = events_tx.send(Event::StateChanged(SessionState::WaitingForUser).into());
}

/// The instruction injected as a synthetic user message ahead of a forced
/// cap wrap-up completion — the Hermes/OpenCode shape
/// (`docs/research/agent-context-reduction-prior-art-2026-07-26.md` §4):
/// stop calling tools and report what was found instead of hard-erroring
/// with the work discarded.
const CAP_SUMMARY_INSTRUCTION: &str = "You have reached the turn limit for this task. Stop here \
     and summarize, without calling any more tools: the relevant files you found (with paths and \
     line numbers), your best partial answer to the question you were asked, and what remains \
     unknown.";

/// Runs one forced, tools-disabled completion for an iteration-cap halt on a
/// role that opts into it (`RoleDefinition::summarize_on_cap`) -- see
/// `halt_turn_loop`'s doc comment for the surrounding decision.
///
/// Folds `arrived_result` -- the real, already-executed result that tripped
/// the guard -- into `rig_history` first (exactly where an ordinary
/// tool-driven turn would put it), then runs one more completion with every
/// tool definition withheld (`RigAgentConfig::allowed_tool_ids` overridden
/// to an empty list, which `rig_tool_definitions` turns into "advertise
/// nothing"), so the model cannot keep exploring even if it tries.
///
/// Returns whether the wrap-up produced a completion at all. `false`
/// (provider failure, or the turn getting cancelled mid-wrap-up) truncates
/// `rig_history` back to exactly what the caller passed in -- no half step
/// -- so `halt_turn_loop`'s ordinary fallback (stash `arrived_result` for
/// `Continue`/a new `UserMessage`) stays correct rather than double-folding
/// it. Truncating to a recorded length, rather than popping a fixed count,
/// is deliberate: `complete_rig_turn` pushes a different number of messages
/// depending on how far the wrap-up got (zero on a provider-request error,
/// two -- the prompt and a partial assistant message -- on cancellation).
#[allow(clippy::too_many_arguments)]
async fn run_cap_summary_turn(
    commands: &mut UnboundedReceiver<Command>,
    inbox: &mut VecDeque<Command>,
    config: &RigAgentConfig,
    environment: &SessionEnvironment,
    extra_sections: &[String],
    rig_history: &mut Vec<Message>,
    clearing: &mut ClearingState,
    arrived_result: &ToolCallResult,
    events_tx: &Sender<ProviderEvent>,
) -> bool {
    let baseline_len = rig_history.len();
    rig_history.push(rig_tool_result_message(arrived_result));

    let mut wrap_up_config = config.clone();
    wrap_up_config.allowed_tool_ids = Some(Vec::new());

    let _ = events_tx.send(Event::StateChanged(SessionState::Running).into());
    let outcome = run_cancellable_turn(
        commands,
        inbox,
        &wrap_up_config,
        environment,
        extra_sections,
        rig_history,
        Message::user(CAP_SUMMARY_INSTRUCTION),
        events_tx,
        clearing,
        || deterministic_rig_response(CAP_SUMMARY_INSTRUCTION),
    )
    .await;

    if outcome.failed || outcome.cancelled {
        rig_history.truncate(baseline_len);
        return false;
    }
    true
}
