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
    pub(crate) inputs_paused: bool,
    pub(crate) activation: VecDeque<String>,
    pub(crate) inputs: super::input::Inputs,
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
    // Paired with the executed tool's id: rig 0.42 requires the tool name
    // on every tool-result message, and the descriptor is gone from
    // `pending_tool_calls` by the time this is flushed.
    pub(crate) pending_halt_result: Option<(ToolCallResult, String)>,

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

    // --- Session identity/configuration and replaceable environment --------
    pub(crate) session_id: SessionId,
    pub(crate) config: RigAgentConfig,
    /// The whole surface at spawn time — what a mid-session switch
    /// (`Command::SetSessionModel`) resolves its target against.
    pub(crate) table: crate::config::ProvidersTable,
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
            inputs: super::input::Inputs::default(),
            activation: VecDeque::new(),
            inputs_paused: false,
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
            table: crate::config::ProvidersTable {
                entries: Vec::new(),
                default_name: String::new(),
            },
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
        table: crate::config::ProvidersTable,
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
            inputs: super::input::Inputs::default(),
            activation: VecDeque::new(),
            inputs_paused: false,
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
            table,
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
            while let Ok(command) = self.commands.try_recv() {
                self.inbox.push_back(command);
            }
            // Lifecycle controls must run before starting queued work, including
            // controls observed while the provider awaited an environment swap.
            if let Some(index) = self
                .inbox
                .iter()
                .position(|command| matches!(command, Command::Shutdown | Command::Cancel { .. }))
            {
                let mut preceding = VecDeque::new();
                for _ in 0..index {
                    match self.inbox.pop_front().unwrap() {
                        Command::SessionInput(input) => self
                            .inputs
                            .accept(input, !self.pending_tool_calls.is_empty()),
                        command => preceding.push_back(command),
                    }
                }
                let control = self.inbox.pop_front().unwrap();
                preceding.append(&mut self.inbox);
                self.inbox = preceding;
                self.record_active_input();
                self.inbox.push_front(control);
            }
            if self.inbox.is_empty() && self.pending_tool_calls.is_empty() {
                self.activate_environment().await;
                if !self.inputs_paused && !self.has_pending_stop() {
                    if let Some(text) = self.inputs.start_next() {
                        self.record_active_input();
                        self.inbox.push_front(Command::UserMessage { text });
                    }
                }
            }
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
                    if self.inputs_paused {
                        continue;
                    }
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
                    if let Some((result, tool_id)) = self.pending_halt_result.take() {
                        self.rig_history
                            .push(rig_tool_result_message(&result, &tool_id));
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
                // Mid-session provider/model switch, latest turn wins: swap
                // what the *next turn* builds with. Announcing the resolved
                // model is `horizon-agentd`'s job (the RPC handler owns
                // `AgentWireEvent::SessionModel`); the loop only fails a
                // switch it cannot resolve, as an ordinary error event.
                Command::SetSessionModel { provider, model } => {
                    if let Err(message) =
                        apply_set_session_model(&mut self.config, &self.table, &provider, &model)
                    {
                        let _ = self.events_tx.send(
                            crate::contract::Event::Error(crate::contract::Error { message })
                                .into(),
                        );
                    }
                }
                Command::SessionInput(input) => {
                    let resume_work = input.resume_work;
                    self.inputs
                        .accept(input, !self.pending_tool_calls.is_empty());
                    if resume_work {
                        self.pause_inputs(false);
                    }
                    self.record_active_input();
                }
                Command::AcknowledgeDelivery { .. } | Command::SendSessionInput { .. } => {}
                Command::ActivateWorktree { base } => {
                    self.activation.push_back(base);
                    if self.pending_tool_calls.is_empty() {
                        self.activate_environment().await;
                    }
                }
                Command::EnvironmentPrepared { .. }
                | Command::EnvironmentActivationFailed { .. } => {}

                crate::contract::Command::Initialize(_) => {
                    // Initialization accepts input without starting a turn.
                    // A synthetic Running event would erase the previous
                    // failed/paused result when a session is restored.
                    let _ = self.events_tx.send(
                        crate::contract::Event::StateChanged(
                            crate::contract::SessionState::WaitingForUser,
                        )
                        .into(),
                    );
                }
                crate::contract::Command::UserMessage { text } => {
                    self.pause_inputs(false);
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
                    if let Some((result, tool_id)) = self.pending_halt_result.take() {
                        self.rig_history
                            .push(rig_tool_result_message(&result, &tool_id));
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
                        self.halt_turn_loop(halt, &result, &descriptor.tool_id)
                            .await;
                        continue;
                    }

                    if fold_batched_tool_result(
                        &mut self.rig_history,
                        &self.pending_tool_calls,
                        &result,
                        &descriptor.tool_id,
                    ) == BatchStep::Continue
                    {
                        continue;
                    }

                    // The whole batch has landed: this is the one tool-driven
                    // turn the batch counts as, so the iteration-cap guard is
                    // recorded exactly once here — never per result, or an
                    // N-call batch would burn the cap N times faster.
                    if let Some(halt) = self.guard.record_tool_turn() {
                        self.halt_turn_loop(halt, &result, &descriptor.tool_id)
                            .await;
                        continue;
                    }

                    let _ = self.events_tx.send(
                        crate::contract::Event::StateChanged(
                            crate::contract::SessionState::Running,
                        )
                        .into(),
                    );
                    let (prompt, injected) = self.inject_task_notification(
                        rig_tool_result_message(&result, &descriptor.tool_id),
                    );
                    self.run_turn(prompt, move || match injected {
                        Some(text) => deterministic_rig_response(&text),
                        None => deterministic_tool_result_response(&result),
                    })
                    .await;
                }
                crate::contract::Command::ContinueTurn => {
                    self.pause_inputs(false);
                    let Some((result, tool_id)) = self.pending_halt_result.take() else {
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
                        self.halt_turn_loop(halt, &result, &tool_id).await;
                        continue;
                    }
                    let _ = self.events_tx.send(
                        crate::contract::Event::StateChanged(
                            crate::contract::SessionState::Running,
                        )
                        .into(),
                    );
                    let (prompt, injected) =
                        self.inject_task_notification(rig_tool_result_message(&result, &tool_id));
                    self.run_turn(prompt, move || match injected {
                        Some(text) => deterministic_rig_response(&text),
                        None => deterministic_tool_result_response(&result),
                    })
                    .await;
                }
                crate::contract::Command::Cancel { .. } => {
                    self.pause_inputs(true);
                    if !self.cancel_outstanding_tool_calls() {
                        // Nothing in flight (no running turn, no pending tool
                        // call) — cancel is a no-op in v1's "cancel whatever
                        // is in flight" semantics.
                        continue;
                    }
                    self.emit_cancelled_turn();
                }
                crate::contract::Command::Shutdown => {
                    self.finish_input(crate::contract::InputResult::Interrupted);
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

/// Swaps a session's per-turn config to a resolved `[[providers]]` entry —
/// `Command::SetSessionModel`'s whole effect, factored out pure so the
/// switch's precedence rules are unit-testable without a session loop.
///
/// `model` resolves against the target entry's alias map FIRST (the picker
/// only offers aliases) and passes through as a raw model id otherwise —
/// the same rule `role.model` follows, so a power user can name a model
/// the entry doesn't alias. The owner-agreed priority is explicit
/// selection > `role.model` > config default: an explicit switch replaces
/// the provider bits wholesale (kind, key variable, base URL, model),
/// while the role's other overrides (tool restrictions, iteration cap)
/// stay, because they are not model-level knobs. Presence of the target
/// entry's key variable is re-read at switch time (a key appearing or
/// disappearing in the environment is honored here, mirroring how the
/// entry's presence was resolved once at build); a target without its key
/// runs the ordinary deterministic fallback, the same behavior a
/// `[provider]`-only config with no key has always had.
///
/// Announcing the resolved model is NOT this fn's job: the RPC handler
/// owns `AgentWireEvent::SessionModel`.
pub(super) fn apply_set_session_model(
    config: &mut RigAgentConfig,
    table: &crate::config::ProvidersTable,
    provider: &str,
    model: &str,
) -> Result<(), String> {
    let Some(entry) = table.entry(provider) else {
        return Err(format!("Unknown provider `{provider}`."));
    };
    if model.is_empty() {
        return Err("A model id is required.".to_string());
    }
    let resolved_model = entry
        .models
        .iter()
        .find(|(alias, _)| alias == model)
        .map(|(_, id)| id.clone())
        .unwrap_or_else(|| model.to_string());
    config.kind = entry.kind;
    config.api_key_env = entry.api_key_env.clone();
    // Same env precedence the entry was built with: the kind's own
    // base-URL variable wins over the file value.
    config.base_url = crate::config::resolve_base_url(
        std::env::var(entry.kind.base_url_env()).ok(),
        entry.base_url.clone(),
    );
    config.api_key_present = std::env::var_os(&entry.api_key_env).is_some();
    config.model = resolved_model;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NamedProviderConfig, ProviderKind, ProvidersTable};

    fn table() -> ProvidersTable {
        ProvidersTable {
            entries: vec![
                NamedProviderConfig {
                    name: "openai".to_string(),
                    kind: ProviderKind::OpenAiCompatible,
                    base_url: Some("https://openai.example.invalid".to_string()),
                    api_key_env: "OPENAI_API_KEY".to_string(),
                    api_key_present: true,
                    models: vec![("fast".to_string(), "m-fast".to_string())],
                },
                NamedProviderConfig {
                    name: "claude".to_string(),
                    kind: ProviderKind::Anthropic,
                    base_url: None,
                    api_key_env: "ANTHROPIC_API_KEY".to_string(),
                    api_key_present: false,
                    models: vec![("opus".to_string(), "m-opus".to_string())],
                },
            ],
            default_name: "openai".to_string(),
        }
    }

    #[test]
    fn switch_resolves_the_alias_and_swaps_the_provider_bits() {
        // `role.model` (or the config default) is what the session was
        // resolved with at spawn; the explicit switch wins over it —
        // the owner-agreed priority: explicit selection > role.model >
        // config default.
        let mut config = RigAgentConfig {
            model: "role-model".to_string(),
            ..Default::default()
        };

        apply_set_session_model(&mut config, &table(), "claude", "opus").unwrap();
        assert_eq!(config.kind, ProviderKind::Anthropic);
        assert_eq!(config.api_key_env, "ANTHROPIC_API_KEY");
        assert_eq!(config.model, "m-opus");
        // Base URL keeps the entry's env precedence (the kind's own
        // variable wins over the file value); compared against the same
        // resolution so the assertion holds whatever the environment
        // carries.
        assert_eq!(
            config.base_url,
            crate::config::resolve_base_url(std::env::var("ANTHROPIC_BASE_URL").ok(), None)
        );
        // Presence re-read at switch time — compared against the same env
        // read, and never the source entry's build-time value.
        assert_eq!(
            config.api_key_present,
            std::env::var_os("ANTHROPIC_API_KEY").is_some()
        );
    }

    #[test]
    fn switch_passes_a_raw_model_id_through_like_role_model_does() {
        let mut config = RigAgentConfig::default();
        apply_set_session_model(&mut config, &table(), "openai", "raw-model-id").unwrap();
        assert_eq!(config.model, "raw-model-id");
        assert_eq!(config.kind, ProviderKind::OpenAiCompatible);
        assert_eq!(
            config.base_url,
            crate::config::resolve_base_url(
                std::env::var("OPENAI_BASE_URL").ok(),
                Some("https://openai.example.invalid".to_string()),
            )
        );
    }

    #[test]
    fn switch_rejects_an_unknown_provider_and_an_empty_model() {
        let mut config = RigAgentConfig::default();
        let error = apply_set_session_model(&mut config, &table(), "typo", "m").unwrap_err();
        assert!(error.contains("Unknown provider `typo`"));
        let error = apply_set_session_model(&mut config, &table(), "openai", "").unwrap_err();
        assert!(error.contains("A model id is required"));
        // A failed switch leaves the previous config untouched — the turn
        // in progress and the next one keep the old selection.
        assert_eq!(config.model, crate::config::RigAgentConfig::default().model);
    }
}
