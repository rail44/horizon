//! Session command coordinator. Inputs owns admission and receipts; Execution
//! owns tool batches and retained halt results. Provider futures remain local
//! to the turn pipeline so commands can cancel them without sharing mutation.

use std::collections::VecDeque;

use super::super::conversation::ConversationHistory;
use crossbeam_channel::Sender;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
    config::RigAgentConfig,
    contract::{Command, OccurrenceId, ProviderEvent, SessionId},
    prompt::SessionEnvironment,
    roles::RoleDefinition,
};

use super::{ClearingState, TurnLoopGuard};

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

/// The session owns command ordering and provider futures. Input admission and
/// retained execution work have separate owners; configuration is independent.
pub(crate) struct SessionLoopState {
    pub(crate) activation: VecDeque<String>,
    pub(crate) inputs: super::input::Inputs,
    // --- Mutable loop state ---------------------------------------------
    /// Canonical conversation; clearing only changes the provider projection.
    pub(crate) rig_history: ConversationHistory,
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
    /// User messages that came while a tool batch was out: each goes into
    /// the history before the turn's next round, so it steers the turn
    /// rather than ending it (ACP's "Prompts during active work").
    pub(crate) steering: Vec<String>,
    pub(crate) execution: super::progress::Execution,
    /// The token budget and the truncation-continue bound.
    pub(crate) guard: TurnLoopGuard,

    /// What customizes this session (`crate::modules`), composed at start.
    pub(crate) modules: Vec<std::sync::Arc<dyn crate::modules::SessionModule>>,
    /// The continue stage's per-turn inputs
    /// (`docs/agent-decision-model-design.md`).
    pub(crate) continuation: super::continuation::ContinuationState,

    // --- Session identity/configuration and replaceable environment --------
    pub(crate) session_id: SessionId,
    pub(crate) config: RigAgentConfig,
    pub(crate) environment: SessionEnvironment,
    /// The prefix's sections and tools, sent as they were at the start.
    pub(crate) resolved: super::super::session_prompt::Resolved,
    /// What the model was last told applies to the session.
    pub(crate) told: super::super::session_prompt::Told,
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
            rig_history: ConversationHistory::default(),
            clearing: ClearingState::disabled(),
            commands,
            task_wake,
            inbox: VecDeque::new(),
            steering: Vec::new(),
            execution: Default::default(),
            guard: TurnLoopGuard::new(None),
            continuation: Default::default(),
            modules: Vec::new(),
            session_id: SessionId::new(),
            config: RigAgentConfig::default(),
            environment: SessionEnvironment::for_workspace_root(None),
            resolved: Default::default(),
            told: Default::default(),
            role: None,
            events_tx,
        }
    }
}

impl SessionLoopState {
    pub(super) fn note_tool_call_reissued(&mut self, identity: crate::contract::ToolCallIdentity) {
        self.execution.reissue(identity);
    }

    /// Constructs the state [`Self::run`] needs, doing the async init
    /// (clearing-state discovery, command bridging, wake registration) that
    /// must happen inside the loop's own runtime.
    #[expect(clippy::too_many_arguments)]
    pub(super) async fn new(
        session_id: SessionId,
        commands_rx: crossbeam_channel::Receiver<Command>,
        events_tx: Sender<ProviderEvent>,
        config: RigAgentConfig,
        environment: SessionEnvironment,
        resolved: super::super::session_prompt::Resolved,
        modules: Vec<std::sync::Arc<dyn crate::modules::SessionModule>>,
        role: Option<&'static RoleDefinition>,
        rig_history: ConversationHistory,
        cleared_occurrence_ids: Vec<OccurrenceId>,
    ) -> Self {
        let mut config = config;
        let mut clearing = super::discover_clearing_state(&mut config).await;
        clearing.seed_cleared(cleared_occurrence_ids);
        // Only where the config and role call for it.
        let continuation = super::continuation::ContinuationState::default();
        Self {
            inputs: super::input::Inputs::default(),
            activation: VecDeque::new(),
            session_id,
            commands: super::bridge_commands(commands_rx),
            task_wake: crate::tools::register_wake(session_id),
            inbox: VecDeque::new(),
            steering: Vec::new(),
            rig_history,
            clearing,
            execution: Default::default(),
            guard: TurnLoopGuard::new(config.turn_token_budget),
            continuation,
            modules,
            config,
            environment,
            told: resolved.told(),
            resolved,
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
            self.prepare_next_input().await;
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
                    self.handle_task_wake().await;
                    continue;
                }
            };

            match command {
                Command::ToolCallReissued(identity) => {
                    self.note_tool_call_reissued(identity);
                }
                Command::ApplySessionModel(selection) => {
                    self.handle_set_session_model(&selection).await;
                }
                Command::SetSessionReasoning { level } => {
                    self.config.reasoning = level;
                    self.announce_reasoning();
                }
                Command::SetSessionModel { .. } => {
                    let _ = self.events_tx.send(
                        crate::contract::Event::Error(crate::contract::Error {
                            message: "Model selection must be resolved by the daemon.".into(),
                        })
                        .into(),
                    );
                }
                Command::SessionInput(input) => {
                    let resume_work = input.resume_work;
                    self.inputs
                        .accept(input, self.execution.has_pending_tools());
                    if resume_work {
                        self.pause_inputs(false);
                    }
                    self.record_active_input();
                }
                Command::AcknowledgeDelivery { .. }
                | Command::SendSessionInput { .. }
                | Command::TitleSession { .. } => {}
                Command::ActivateWorktree { base } => {
                    self.activation.push_back(base);
                    if !self.execution.has_pending_tools() {
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
                    self.handle_user_message(text).await;
                }
                crate::contract::Command::ToolCallResult(result) => {
                    self.handle_tool_result(result).await;
                }
                crate::contract::Command::Cancel { .. } => {
                    // A cancel drops the messages not inserted yet.
                    self.steering.clear();
                    self.pause_inputs(true);
                    if !self.cancel_outstanding_tool_calls().await {
                        // Admission remains paused even without a tool batch;
                        // there is no active turn to finish.
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
                crate::contract::Command::Suspend => {
                    self.finish_input(crate::contract::InputResult::Interrupted);
                    let _ = self.events_tx.send(
                        crate::contract::Event::StateChanged(
                            crate::contract::SessionState::Suspended,
                        )
                        .into(),
                    );
                    break;
                }
                crate::contract::Command::ApproveToolCall { .. }
                | crate::contract::Command::DenyToolCall { .. }
                | crate::contract::Command::ToolCallsSettled { .. } => {}
            }
        }

        crate::tools::unregister_wake(self.session_id);
    }
}

impl SessionLoopState {
    /// Apply a host-resolved snapshot at a turn boundary, then acknowledge it.
    async fn handle_set_session_model(
        &mut self,
        selection: &crate::config::ResolvedModelSelection,
    ) {
        selection.apply(&mut self.config);
        self.rediscover_clearing_window().await;
        let _ = self
            .events_tx
            .send(ProviderEvent::session_model(selection.model().to_owned()));
        let _ = self.events_tx.send(ProviderEvent::session_selection(
            selection.requested_provider().to_owned(),
            selection.requested_model().to_owned(),
        ));
        // The new model may take other levels.
        self.announce_reasoning();
    }

    /// Says which level the session's requests now send, and which the
    /// model takes.
    pub(crate) fn announce_reasoning(&self) {
        let _ = self.events_tx.send(ProviderEvent::SessionReasoning(
            crate::providers::rig::reasoning_selection(&self.config),
        ));
    }

    /// Re-reads the effective context window for the model this session now
    /// runs, keeping the rest of the clearing state
    /// ([`ClearingState::adopt_window`]). Without this a switch would keep
    /// clearing against the previous model's window — with a smaller model
    /// that puts the trigger above its whole window, so the session runs to
    /// its context ceiling instead of clearing.
    pub(crate) async fn rediscover_clearing_window(&mut self) {
        let window = super::discover_effective_window(&mut self.config).await;
        self.clearing.adopt_window(window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NamedProviderConfig, ProvidersTable, Vendor};

    fn apply_set_session_model(
        config: &mut RigAgentConfig,
        table: &ProvidersTable,
        provider: &str,
        model: &str,
    ) -> Result<(), String> {
        crate::config::resolve_model_selection(table, provider, model)?.apply(config);
        Ok(())
    }

    fn table() -> ProvidersTable {
        ProvidersTable {
            entries: vec![
                NamedProviderConfig {
                    name: "openai".to_string(),
                    vendor: Vendor::openai(),
                    base_url: Some("https://openai.example.invalid".to_string()),
                    api_key_env: "OPENAI_API_KEY".to_string(),
                    api_key_present: true,
                },
                NamedProviderConfig {
                    name: "claude".to_string(),
                    vendor: Vendor::resolve("anthropic").unwrap(),
                    base_url: None,
                    api_key_env: "ANTHROPIC_API_KEY".to_string(),
                    api_key_present: false,
                },
            ],
            default_model: None,
        }
    }

    #[test]
    fn switch_sets_the_model_id_and_swaps_the_provider_bits() {
        // `role.model` (or the config default) is what the session was
        // resolved with at spawn; the explicit switch wins over it —
        // the owner-agreed priority: explicit selection > role.model >
        // config default.
        let mut config = RigAgentConfig {
            model: "role-model".to_string(),
            ..Default::default()
        };

        apply_set_session_model(&mut config, &table(), "claude", "m-opus").unwrap();
        assert_eq!(config.vendor, Vendor::resolve("anthropic").unwrap());
        assert_eq!(config.api_key_env, "ANTHROPIC_API_KEY");
        assert_eq!(config.model, "m-opus");
        // The entry's own base URL, unset here: rig's default applies.
        assert_eq!(config.base_url, None);
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
        assert_eq!(config.vendor, Vendor::openai());
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://openai.example.invalid")
        );
    }

    /// A running session applies a resolved catalog snapshot without keeping
    /// its own catalog, and acknowledges only after updating next-turn config.
    #[tokio::test]
    async fn the_session_loop_applies_a_switch_to_its_own_next_turn_config() {
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let mut state = SessionLoopState {
            events_tx,
            ..Default::default()
        };
        state.config.model = "role-model".to_string();

        let selection =
            crate::config::resolve_model_selection(&table(), "claude", "m-opus").unwrap();
        state.handle_set_session_model(&selection).await;
        assert_eq!(state.config.model, "m-opus");
        assert_eq!(state.config.vendor, Vendor::resolve("anthropic").unwrap());
        assert_eq!(
            events_rx.try_recv().unwrap(),
            ProviderEvent::SessionModel("m-opus".into())
        );
        assert_eq!(
            events_rx.try_recv().unwrap(),
            ProviderEvent::session_selection("claude".into(), "m-opus".into())
        );
        // The new model's levels, announced with the level the session
        // keeps wanting.
        let ProviderEvent::SessionReasoning(reasoning) = events_rx.try_recv().unwrap() else {
            panic!("a switch announces the session's reasoning");
        };
        assert_eq!(
            reasoning.choices,
            crate::config::ReasoningLevel::ALL.to_vec()
        );
        assert_eq!(reasoning.level, Some(state.config.reasoning));
        assert!(events_rx.try_recv().is_err());
    }

    /// A switch re-reads the window for the model the session now runs.
    /// The `claude` entry is anthropic, which declares no window without
    /// issuing a request, so the switch ends with clearing disabled — and
    /// the frozen cleared set and the last measured input size, both of
    /// which describe history the switch did not touch, survive it.
    #[tokio::test]
    async fn a_switch_rediscovers_the_window_and_keeps_the_frozen_clearing_state() {
        let (events_tx, _events_rx) = crossbeam_channel::unbounded();
        let mut state = SessionLoopState {
            events_tx,
            clearing: ClearingState::new(Some(500_000)),
            ..Default::default()
        };
        state
            .clearing
            .seed_cleared(vec![crate::contract::OccurrenceId("call-0".to_string())]);
        state.clearing.record_input_tokens(400_000);

        let selection =
            crate::config::resolve_model_selection(&table(), "claude", "m-opus").unwrap();
        state.handle_set_session_model(&selection).await;

        assert_eq!(
            state.clearing.effective_window_tokens(),
            None,
            "the previous model's window must not outlive the switch"
        );
        assert_eq!(state.clearing.latest_input_tokens(), 400_000);
        assert!(
            state
                .clearing
                .cleared()
                .contains(&crate::contract::OccurrenceId("call-0".to_string())),
            "a frozen pass stays frozen across a switch"
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
