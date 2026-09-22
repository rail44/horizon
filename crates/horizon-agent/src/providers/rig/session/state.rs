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

use super::{ClearingState, ToolCallDescriptor, TurnLoopGuard};

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
    /// The `[[moa]]` surface at spawn time, resolved the same way.
    pub(crate) moa_table: crate::config::MoaTable,
    /// The owner messages and answers a Mixture-of-Agents pass hands its
    /// proposers. Maintained for every session (it costs one push per
    /// message) so switching into a `[[moa]]` entry mid-session starts with
    /// the conversation that already happened.
    pub(crate) moa_conversation: super::moa::MoaConversation,
    /// The proposals the current turn's provider rounds carry, `None`
    /// outside a MoA turn.
    pub(crate) moa_turn: Option<super::moa::MoaTurn>,
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
            moa_table: crate::config::MoaTable::default(),
            moa_conversation: super::moa::MoaConversation::default(),
            moa_turn: None,
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
        moa_table: crate::config::MoaTable,
        environment: SessionEnvironment,
        extra_sections: Vec<String>,
        role: Option<&'static RoleDefinition>,
        rig_history: Vec<Message>,
        cleared_call_ids: Vec<ToolCallId>,
        memory_document: Option<MemoryDocument>,
        moa_conversation: super::moa::MoaConversation,
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
            moa_table,
            moa_conversation,
            moa_turn: None,
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
                // Mid-session provider/model switch, latest turn wins: swap
                // what the *next turn* builds with. Announcing the resolved
                // model is `horizon-agentd`'s job (the RPC handler owns
                // `AgentWireEvent::SessionModel`); the loop only fails a
                // switch it cannot resolve, as an ordinary error event.
                Command::SetSessionModel { provider, model } => {
                    self.handle_set_session_model(&provider, &model).await;
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
                    self.handle_user_message(text).await;
                }
                crate::contract::Command::ToolCallResult(result) => {
                    self.handle_tool_result(result).await;
                }
                crate::contract::Command::ContinueTurn => {
                    self.continue_halted_turn().await;
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

impl SessionLoopState {
    /// `Command::SetSessionModel`'s whole loop-side effect: swap what the
    /// next turn builds with, or report a switch the loop cannot resolve as
    /// an ordinary error event. Announcing the resolved model is NOT here —
    /// the RPC handler owns `AgentWireEvent::SessionModel`. Named and
    /// unit-tested separately because the command pump's arm is otherwise
    /// the one untested join between the daemon-side forward and the next
    /// turn's config.
    async fn handle_set_session_model(&mut self, provider: &str, model: &str) {
        match apply_set_session_model(
            &mut self.config,
            &self.table,
            &self.moa_table,
            provider,
            model,
        ) {
            Ok(()) => self.rediscover_clearing_window().await,
            Err(message) => {
                let _ = self
                    .events_tx
                    .send(crate::contract::Event::Error(crate::contract::Error { message }).into());
            }
        }
    }

    /// Re-reads the effective context window for the model this session now
    /// runs, keeping the rest of the clearing state
    /// ([`ClearingState::adopt_window`]). Without this a switch would keep
    /// clearing against the previous model's window — with a smaller model
    /// that puts the trigger above its whole window, so the session runs to
    /// its context ceiling instead of clearing.
    pub(crate) async fn rediscover_clearing_window(&mut self) {
        let window = super::discover_effective_window(&self.config).await;
        self.clearing.adopt_window(window);
    }
}

/// Swaps a session's per-turn config to a resolved `[[providers]]` entry —
/// `Command::SetSessionModel`'s whole effect, factored out pure so the
/// switch's precedence rules are unit-testable without a session loop.
///
/// `model` is a model id (a declared one or a live `/models` id): it swaps
/// the session's per-turn config onto the target entry's kind/key/base URL
/// and sets that id verbatim. The owner-agreed priority is explicit
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
/// `provider` names either a `[[providers]]` entry or the reserved
/// [`crate::config::MOA_PROVIDER_NAME`] group, in which case `model` is a
/// `[[moa]]` entry name: the session then runs that entry's aggregator and
/// its owner messages open a pass. Switching to any other provider clears
/// the pass.
pub(super) fn apply_set_session_model(
    config: &mut RigAgentConfig,
    table: &crate::config::ProvidersTable,
    moa_table: &crate::config::MoaTable,
    provider: &str,
    model: &str,
) -> Result<(), String> {
    if model.is_empty() {
        return Err("A model id is required.".to_string());
    }
    if provider == crate::config::MOA_PROVIDER_NAME {
        let Some(entry) = moa_table.entry(model) else {
            return Err(format!("Unknown moa entry `{model}`."));
        };
        // An unavailable aggregator would answer from the deterministic
        // fallback responder, which reads in the pane as the model's own
        // answer. Refused instead, the way a key-less `[[providers]]` entry
        // is not offered for selection.
        if !entry.aggregator.api_key_present {
            return Err(format!(
                "moa entry `{model}` is unavailable: {}.",
                crate::tools::moa::unavailable_reason(&entry.aggregator)
            ));
        }
        if !crate::config::apply_moa_selection(config, table, entry) {
            return Err(format!(
                "moa entry `{model}` names no provider `{}`.",
                entry.aggregator.provider
            ));
        }
        return Ok(());
    }
    let Some(entry) = table.entry(provider) else {
        return Err(format!("Unknown provider `{provider}`."));
    };
    crate::config::apply_provider_entry(config, entry, model);
    config.moa = None;
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
                    default_model: Some("m-fast".to_string()),
                },
                NamedProviderConfig {
                    name: "claude".to_string(),
                    kind: ProviderKind::Anthropic,
                    base_url: None,
                    api_key_env: "ANTHROPIC_API_KEY".to_string(),
                    api_key_present: false,
                    default_model: Some("m-opus".to_string()),
                },
            ],
            default_name: "openai".to_string(),
        }
    }

    /// A member on `provider`, available or not. `openai` in [`table`] has
    /// its key, `claude` does not.
    fn moa_member(provider: &str, model: &str, api_key_present: bool) -> crate::config::MoaMember {
        crate::config::MoaMember {
            api_key_present,
            api_key_env: format!("{}_API_KEY", provider.to_uppercase()),
            ..crate::config::MoaMember::new(provider.to_string(), model.to_string())
        }
    }

    /// `mix` aggregates on the available entry; `stranded` aggregates on the
    /// key-less one.
    fn moa_table() -> crate::config::MoaTable {
        crate::config::MoaTable {
            entries: vec![
                crate::config::MoaEntry {
                    name: "mix".to_string(),
                    aggregator: moa_member("openai", "m-aggregate", true),
                    proposers: vec![
                        moa_member("openai", "m-fast", true),
                        moa_member("claude", "m-opus", false),
                    ],
                },
                crate::config::MoaEntry {
                    name: "stranded".to_string(),
                    aggregator: moa_member("claude", "m-opus", false),
                    proposers: vec![moa_member("openai", "m-fast", true)],
                },
            ],
        }
    }

    /// A MoA entry whose aggregator's key variable is unset is refused
    /// rather than installed: a session on it would answer from the
    /// deterministic fallback responder.
    #[test]
    fn selecting_a_moa_entry_with_an_unavailable_aggregator_is_refused() {
        let mut config = RigAgentConfig::default();
        let before = config.model.clone();
        let error = apply_set_session_model(&mut config, &table(), &moa_table(), "moa", "stranded")
            .unwrap_err();
        assert!(error.contains("unavailable"), "{error}");
        assert!(error.contains("CLAUDE_API_KEY"), "{error}");
        assert_eq!(config.model, before);
        assert!(config.moa.is_none());
    }

    /// Selecting the `moa` group points the session at the entry's
    /// aggregator and installs the pass; selecting an ordinary provider
    /// afterwards clears it.
    #[test]
    fn selecting_a_moa_entry_installs_the_pass_and_switching_away_clears_it() {
        let mut config = RigAgentConfig::default();

        apply_set_session_model(&mut config, &table(), &moa_table(), "moa", "mix").unwrap();
        assert_eq!(config.model, "m-aggregate");
        assert_eq!(config.kind, ProviderKind::OpenAiCompatible);
        assert_eq!(config.api_key_env, "OPENAI_API_KEY");
        let pass = config.moa.clone().expect("the pass is installed");
        assert_eq!(pass.name, "mix");
        assert_eq!(
            pass.proposers
                .iter()
                .map(|member| member.model.as_str())
                .collect::<Vec<_>>(),
            vec!["m-fast", "m-opus"]
        );

        apply_set_session_model(&mut config, &table(), &moa_table(), "openai", "m-fast").unwrap();
        assert_eq!(config.model, "m-fast");
        assert!(config.moa.is_none());
    }

    #[test]
    fn selecting_an_unknown_moa_entry_is_an_error_that_changes_nothing() {
        let mut config = RigAgentConfig::default();
        let before = config.model.clone();
        let error = apply_set_session_model(&mut config, &table(), &moa_table(), "moa", "typo")
            .unwrap_err();
        assert!(error.contains("Unknown moa entry `typo`"), "{error}");
        assert_eq!(config.model, before);
        assert!(config.moa.is_none());
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

        apply_set_session_model(&mut config, &table(), &moa_table(), "claude", "m-opus").unwrap();
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
        apply_set_session_model(
            &mut config,
            &table(),
            &moa_table(),
            "openai",
            "raw-model-id",
        )
        .unwrap();
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

    /// The loop-side pump arm, driven directly through a real
    /// `SessionLoopState`: the command mutates the state the next turn
    /// reads (`run_turn` → `complete_rig_turn`), and an unresolvable
    /// switch surfaces as an error event on the provider channel. This is
    /// the join between the daemon-side forward (connection test) and the
    /// next turn's config — the one piece a live provider-call round-trip
    /// cannot observe hermetically, since a keyed turn's client
    /// construction reads the environment per turn by design (mutating env
    /// in a test would race the parallel suite).
    #[tokio::test]
    async fn the_session_loop_applies_a_switch_to_its_own_next_turn_config() {
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let mut state = SessionLoopState {
            events_tx,
            table: table(),
            ..Default::default()
        };
        state.config.model = "role-model".to_string();

        state.handle_set_session_model("claude", "m-opus").await;
        assert_eq!(state.config.model, "m-opus");
        assert_eq!(state.config.kind, ProviderKind::Anthropic);
        assert!(events_rx.try_recv().is_err(), "no error event on success");

        // An unresolvable switch leaves the config untouched and reports
        // the failure on the provider channel instead.
        state.handle_set_session_model("typo", "m").await;
        assert_eq!(state.config.model, "m-opus");
        let received = events_rx.try_recv().unwrap();
        assert!(
            matches!(received.event, crate::contract::Event::Error(_)),
            "{received:?}"
        );
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
            table: table(),
            clearing: ClearingState::new(Some(500_000), 60),
            ..Default::default()
        };
        state
            .clearing
            .seed_cleared(vec![crate::contract::ToolCallId("call-0".to_string())]);
        state.clearing.record_input_tokens(400_000);

        state.handle_set_session_model("claude", "m-opus").await;

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
                .contains(&crate::contract::ToolCallId("call-0".to_string())),
            "a frozen pass stays frozen across a switch"
        );
    }

    #[test]
    fn switch_rejects_an_unknown_provider_and_an_empty_model() {
        let mut config = RigAgentConfig::default();
        let error =
            apply_set_session_model(&mut config, &table(), &moa_table(), "typo", "m").unwrap_err();
        assert!(error.contains("Unknown provider `typo`"));
        let error =
            apply_set_session_model(&mut config, &table(), &moa_table(), "openai", "").unwrap_err();
        assert!(error.contains("A model id is required"));
        // A failed switch leaves the previous config untouched — the turn
        // in progress and the next one keep the old selection.
        assert_eq!(config.model, crate::config::RigAgentConfig::default().model);
    }
}
