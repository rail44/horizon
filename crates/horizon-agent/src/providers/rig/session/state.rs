//! [`SessionLoopState`] — the mutable state `run_session_loop` threads
//! through every turn, bundled into a struct so the turn-execution
//! pipeline methods (in [`super::turn`]) take `&mut self` instead of the
//! 10+ individual arguments the free-function form accumulated.

use std::collections::{HashMap, HashSet, VecDeque};

use crossbeam_channel::Sender;
use rig_core::completion::Message;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
    config::RigAgentConfig,
    contract::{Command, ProviderEvent, SessionId, ToolCallId, ToolCallResult},
    prompt::SessionEnvironment,
    roles::RoleDefinition,
    tools::MemoryDocument,
};

use super::turn::{fold_batched_tool_result, BatchStep};
use super::{
    deterministic_rig_response, deterministic_tool_result_response, rig_tool_result_message,
    tool_result_fingerprint, ClearingState, ToolCallDescriptor, TurnLoopGuard,
};

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

/// The mutable state `run_session_loop` carries across every iteration of
/// the loop, plus the read-only inputs shared for the session's lifetime.
///
/// The nine fields below `rig_history` … `pending_halt_result` are the
/// mutable state that was previously nine `let mut` locals in
/// `run_session_loop`; the remaining fields are the read-only parameters
/// the turn pipeline helpers borrowed via `&` / `&mut` on every call.
/// Bundling them lets the pipeline methods in [`super::turn`] take
/// `&mut self` instead of threading each one as a separate argument.
pub(crate) struct SessionLoopState {
    // --- Mutable loop state ---------------------------------------------
    /// The rig conversation history, grown and cleared as turns run.
    pub(crate) rig_history: Vec<Message>,
    /// Tier 1 compaction state (`docs/agent-compaction-design.md`).
    pub(crate) clearing: ClearingState,
    /// Commands forwarded from the crossbeam channel onto a tokio channel
    /// so the loop can `select!` between a command and an in-flight turn.
    pub(crate) commands: UnboundedReceiver<Command>,
    /// Signalled whenever one of this session's background `task` children
    /// finishes (`tools::explore`).
    pub(crate) task_wake: UnboundedReceiver<()>,
    /// Commands observed mid-turn and queued for replay after the turn.
    pub(crate) inbox: VecDeque<Command>,
    /// Every tool call whose result is still outstanding.
    pub(crate) pending_tool_calls: HashMap<ToolCallId, ToolCallDescriptor>,
    /// Call ids whose real results should be silently dropped on arrival.
    pub(crate) cancelled_call_ids: HashSet<ToolCallId>,
    /// Iteration-cap + doom-loop guard.
    pub(crate) guard: TurnLoopGuard,
    /// The real, already-executed tool result a guard halt stashed instead
    /// of folding into `rig_history` right away — see `halt_turn_loop`.
    pub(crate) pending_halt_result: Option<ToolCallResult>,

    // --- Standing-agent memory (`docs/standing-agent-memory-design.md`) ----
    /// The current memory document, maintained incrementally as
    /// `memory.update` results arrive. Seeded from the event log at spawn;
    /// the provider-view projection prepends it (replacing old history) for
    /// standing roles. `None` for non-standing roles (no memory mechanism).
    pub(crate) memory: Option<MemoryDocument>,
    /// Whether the current user-turn's memory checkpoint is satisfied — i.e.
    /// a `memory.update` call (Updated or Skipped) has landed this turn.
    /// Reset to `false` when a new user message opens a turn.
    pub(crate) memory_satisfied: bool,
    /// Whether a checkpoint reminder has already been injected this turn —
    /// bounds the checkpoint to at most one reminder, then a missed event.
    pub(crate) memory_reminded: bool,

    // --- Read-only inputs (fixed for the session's lifetime) ------------
    pub(crate) session_id: SessionId,
    pub(crate) config: RigAgentConfig,
    pub(crate) environment: SessionEnvironment,
    pub(crate) extra_sections: Vec<String>,
    pub(crate) role: Option<&'static RoleDefinition>,
    pub(crate) events_tx: Sender<ProviderEvent>,
}

impl Default for SessionLoopState {
    fn default() -> Self {
        let (_, commands) = tokio::sync::mpsc::unbounded_channel::<Command>();
        let (_, task_wake) = tokio::sync::mpsc::unbounded_channel::<()>();
        let (events_tx, _) = crossbeam_channel::unbounded::<ProviderEvent>();
        Self {
            rig_history: Vec::new(),
            clearing: ClearingState::disabled(),
            commands,
            task_wake,
            inbox: VecDeque::new(),
            pending_tool_calls: HashMap::new(),
            cancelled_call_ids: HashSet::new(),
            guard: TurnLoopGuard::new(0, 0),
            pending_halt_result: None,
            memory: None,
            memory_satisfied: false,
            memory_reminded: false,
            session_id: SessionId::new(),
            config: RigAgentConfig::default(),
            environment: SessionEnvironment::for_workspace_root(None),
            extra_sections: Vec::new(),
            role: None,
            events_tx,
        }
    }
}

impl SessionLoopState {
    /// Constructs the state `run_session_loop` needs, doing the async init
    /// (clearing-state discovery, command bridging, wake registration) that
    /// must happen inside the loop's own runtime.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn new(
        session_id: SessionId,
        commands_rx: crossbeam_channel::Receiver<Command>,
        events_tx: Sender<ProviderEvent>,
        config: RigAgentConfig,
        environment: SessionEnvironment,
        extra_sections: Vec<String>,
        role: Option<&'static RoleDefinition>,
        rig_history: Vec<Message>,
        cleared_call_ids: Vec<ToolCallId>,
        memory_document: Option<MemoryDocument>,
    ) -> Self {
        let mut clearing = super::discover_clearing_state(&config).await;
        clearing.seed_cleared(cleared_call_ids);
        // A standing role seeds its memory document from the event log; a
        // non-standing role has no memory mechanism, so `memory` stays `None`
        // and the projection skips the memory prepend entirely.
        let memory = if role.is_some_and(|r| r.standing) {
            Some(memory_document.unwrap_or_default())
        } else {
            None
        };
        Self {
            session_id,
            commands: super::bridge_commands(commands_rx),
            task_wake: crate::tools::register_wake(session_id),
            inbox: VecDeque::new(),
            rig_history,
            clearing,
            pending_tool_calls: HashMap::new(),
            cancelled_call_ids: HashSet::new(),
            guard: TurnLoopGuard::new(config.iteration_cap, config.doom_loop_window),
            pending_halt_result: None,
            memory,
            memory_satisfied: false,
            memory_reminded: false,
            config,
            environment,
            extra_sections,
            role,
            events_tx,
        }
    }

    /// The loop body: pick the next input, then dispatch by kind. The
    /// turn-execution pipeline (`run_cancellable_turn` →
    /// `handle_truncation_recovery` → `apply_turn_outcome`) is a single
    /// `self.run_turn(…)` call (see [`super::turn`]); each command arm does
    /// only its own arm-specific setup before calling it.
    pub(super) async fn run(&mut self) {
        loop {
            let next = match self.inbox.pop_front() {
                Some(command) => Next::Command(command),
                None => tokio::select! {
                    maybe_command = self.commands.recv() => match maybe_command {
                        Some(command) => Next::Command(command),
                        None => Next::Closed,
                    },
                    Some(()) = self.task_wake.recv() => Next::TaskWake,
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
                    if !self.pending_tool_calls.is_empty() {
                        continue;
                    }
                    let Some(text) = crate::tools::take_notification(self.session_id) else {
                        continue;
                    };
                    // The same flush `Command::UserMessage` performs: a result
                    // a guard halt stashed still has to land in `rig_history`
                    // before the next request, or the API rejects an assistant
                    // `tool_calls` message with no matching result.
                    if let Some(result) = self.pending_halt_result.take() {
                        self.rig_history.push(rig_tool_result_message(&result));
                    }
                    self.guard.reset();
                    self.memory_satisfied = false;
                    self.memory_reminded = false;
                    let _ = self.events_tx.send(
                        crate::contract::Event::StateChanged(
                            crate::contract::SessionState::Running,
                        )
                        .into(),
                    );
                    let _ = self
                        .events_tx
                        .send(crate::tools::notification_event(text.clone()).into());
                    let fallback_text = text.clone();
                    self.run_turn(Message::user(text), move || {
                        deterministic_rig_response(&fallback_text)
                    })
                    .await;
                    continue;
                }
            };

            match command {
                crate::contract::Command::Initialize(_) => {
                    let _ = self.events_tx.send(
                        crate::contract::Event::StateChanged(
                            crate::contract::SessionState::Running,
                        )
                        .into(),
                    );
                    let _ = self.events_tx.send(
                        crate::contract::Event::StateChanged(
                            crate::contract::SessionState::WaitingForUser,
                        )
                        .into(),
                    );
                }
                crate::contract::Command::UserMessage { text } => {
                    // A user message starts a new interaction rather than
                    // joining the previous turn's tool batch. This command can
                    // arrive while any kind of tool is still running or
                    // awaiting approval, so retire the whole old batch before
                    // asking the provider to handle the new message. Otherwise
                    // those old call ids remain in `pending_tool_calls` and a
                    // result from the new turn is mistaken for a non-final
                    // member of the old batch, leaving the session waiting
                    // forever.
                    if self.cancel_outstanding_tool_calls() {
                        self.emit_cancelled_turn();
                    }
                    // Typing past a halt instead of clicking Continue: the
                    // real result a guard halt stashed still has to land in
                    // `rig_history` before the next request, or the API
                    // rejects it (an assistant `tool_calls` message with no
                    // matching result). A no-op when there's nothing pending.
                    if let Some(result) = self.pending_halt_result.take() {
                        self.rig_history.push(rig_tool_result_message(&result));
                    }
                    // A fresh user message starts a new interaction: both loop
                    // guards below count/track only *tool-driven* turns since
                    // the last user message.
                    self.guard.reset();
                    self.memory_satisfied = false;
                    self.memory_reminded = false;
                    let _ = self.events_tx.send(
                        crate::contract::Event::StateChanged(
                            crate::contract::SessionState::Running,
                        )
                        .into(),
                    );
                    let _ = self.events_tx.send(
                        crate::contract::Event::MessageCommitted(crate::contract::Message {
                            role: crate::contract::MessageRole::User,
                            text: text.clone(),
                        })
                        .into(),
                    );
                    let (prompt, injected) =
                        self.inject_task_notification(Message::user(text.clone()));
                    let fallback_text = injected.unwrap_or(text);
                    self.run_turn(prompt, move || deterministic_rig_response(&fallback_text))
                        .await;
                }
                crate::contract::Command::ToolCallResult(result) => {
                    if self.cancelled_call_ids.remove(&result.call_id) {
                        // A result arriving after its turn was cancelled is
                        // accepted and silently dropped, per contract. This
                        // also covers the rest of a cancelled batch: `Cancel`
                        // drains every still-outstanding call id into
                        // `cancelled_call_ids` (below), so each of their real
                        // results, arriving later, lands here and is dropped
                        // rather than starting a turn.
                        continue;
                    }
                    let Some(descriptor) = self.pending_tool_calls.remove(&result.call_id) else {
                        // Unsolicited (duplicate or stale) result: no pending
                        // tool call under this id. Running a turn from it would
                        // append an orphan tool-result message to rig_history —
                        // the next OpenAI request rejects a tool result with no
                        // matching assistant tool call — and stray results
                        // must not advance the loop guards. Accepted and
                        // silently dropped.
                        continue;
                    };

                    // Standing-agent memory (`docs/standing-agent-memory-
                    // design.md`): a `memory.update` result applies the
                    // parsed digest to the session's memory document (the
                    // tool handler already validated it and returned a
                    // confirmation — this is the state-mutation half) and
                    // emits the `MemoryDigest` event for persistence and
                    // transcript display. Both `Updated` and `Skipped`
                    // (no_update) satisfy the turn-end checkpoint.
                    if descriptor.tool_id == crate::tools::MEMORY_UPDATE_TOOL_ID {
                        if let Some(memory) = self.memory.as_mut() {
                            if let Ok(digest) = crate::tools::parse_update(&descriptor.args) {
                                memory.apply(&digest);
                                let _ = self
                                    .events_tx
                                    .send(crate::contract::Event::MemoryDigest(digest).into());
                                self.memory_satisfied = true;
                            }
                        }
                    }

                    // Doom-loop fingerprinting is per *result* (every call's
                    // outcome must be checked, not just the batch's last), so
                    // it runs unconditionally here — before deciding whether
                    // this is the last outstanding result of the current
                    // batch.
                    let fingerprint = tool_result_fingerprint(
                        &descriptor.tool_id,
                        &descriptor.args,
                        &result.output,
                    );
                    if let Some(halt) = self.guard.record_fingerprint(fingerprint) {
                        // Stop instead of running another turn. The arrived
                        // result is real — its tool already executed — so it
                        // is recorded as-is; only *other* still-pending calls
                        // get the cancelled treatment.
                        self.halt_turn_loop(halt, &result).await;
                        continue;
                    }

                    if fold_batched_tool_result(
                        &mut self.rig_history,
                        &self.pending_tool_calls,
                        &result,
                    ) == BatchStep::Continue
                    {
                        continue;
                    }

                    // The whole batch has landed: this is the one tool-driven
                    // turn the batch counts as, so the iteration-cap guard is
                    // recorded exactly once here — never per result, or an
                    // N-call batch would burn the cap N times faster.
                    if let Some(halt) = self.guard.record_tool_turn() {
                        self.halt_turn_loop(halt, &result).await;
                        continue;
                    }

                    let _ = self.events_tx.send(
                        crate::contract::Event::StateChanged(
                            crate::contract::SessionState::Running,
                        )
                        .into(),
                    );
                    let (prompt, injected) =
                        self.inject_task_notification(rig_tool_result_message(&result));
                    self.run_turn(prompt, move || match injected {
                        Some(text) => deterministic_rig_response(&text),
                        None => deterministic_tool_result_response(&result),
                    })
                    .await;
                }
                crate::contract::Command::ContinueTurn => {
                    let Some(result) = self.pending_halt_result.take() else {
                        // Nothing halted to resume: a safe no-op. Covers a
                        // stale Continue arriving after a fresh user message
                        // already flushed the pending result, a Continue sent
                        // to an idle/never-halted session, and — critically —
                        // a resumed session right after bootstrap: replay
                        // never populates `pending_halt_result` on its own, so
                        // a persisted session that ended halted stays halted
                        // (waiting-for-user) rather than auto-resuming.
                        continue;
                    };
                    self.guard.reset();
                    // Counts as the resumed turn's one tool-driven turn, the
                    // same as the `Command::ToolCallResult` arm above would
                    // have — keeps the guard meaningful even if Continue is
                    // clicked repeatedly on a genuinely runaway loop: it can
                    // re-trip after another full `iteration_cap` turns rather
                    // than being permanently defeated by one reset.
                    if let Some(halt) = self.guard.record_tool_turn() {
                        self.halt_turn_loop(halt, &result).await;
                        continue;
                    }
                    let _ = self.events_tx.send(
                        crate::contract::Event::StateChanged(
                            crate::contract::SessionState::Running,
                        )
                        .into(),
                    );
                    let (prompt, injected) =
                        self.inject_task_notification(rig_tool_result_message(&result));
                    self.run_turn(prompt, move || match injected {
                        Some(text) => deterministic_rig_response(&text),
                        None => deterministic_tool_result_response(&result),
                    })
                    .await;
                }
                crate::contract::Command::Cancel { .. } => {
                    if !self.cancel_outstanding_tool_calls() {
                        // Nothing in flight (no running turn, no pending tool
                        // call) — cancel is a no-op in v1's "cancel whatever
                        // is in flight" semantics.
                        continue;
                    }
                    self.emit_cancelled_turn();
                }
                crate::contract::Command::Shutdown => {
                    let _ = self.events_tx.send(
                        crate::contract::Event::StateChanged(
                            crate::contract::SessionState::Terminated,
                        )
                        .into(),
                    );
                    break;
                }
                crate::contract::Command::ApproveToolCall { .. }
                | crate::contract::Command::DenyToolCall { .. } => {}
            }
        }

        crate::tools::unregister_wake(self.session_id);
    }
}
