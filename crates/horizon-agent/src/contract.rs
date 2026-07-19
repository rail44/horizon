use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

use crossbeam_channel::{Receiver, Sender};

use uuid::Uuid;

use crate::config::AgentConfig;
use crate::roles::RoleId;

/// This crate's own session identifier: a UUID newtype that serializes as a
/// bare UUID string (serde's transparent treatment of one-field tuple
/// structs) — the shape a future wire/IPC boundary will use (see
/// `docs/agent-runtime-split-design.md`). Horizon has its own shared
/// `session::SessionId` (used across terminal and agent sessions alike) —
/// this crate cannot depend on it (that's the whole point of the split), so
/// the two are distinct types connected by `From` impls at the seam in
/// Horizon's `agent` module.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct ProviderId(pub String);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct RequestId(pub String);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
pub struct ToolCallId(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct StartSession {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
    /// `None` for a role-less session (unchanged behavior). `Some` must
    /// already have been validated by the caller (`ProviderRegistry::
    /// start_session`) -- see `roles`'s module doc on never silently
    /// degrading an unresolvable role id to role-less.
    pub role_id: Option<RoleId>,
    /// This session's real working directory -- for an isolated session,
    /// the isolated worktree (already resolved by the caller before this
    /// reaches a provider), not wherever the daemon process happens to be
    /// running from. `None` when no root is available at all. Consumed by
    /// the rig provider to build [`crate::prompt::SessionEnvironment`]
    /// (`providers::rig::session::spawn_rig_session`), so the system
    /// prompt's "Working directory" line and the skills listing
    /// (`providers::rig::session::session_extra_sections`) both reflect the
    /// session's actual root instead of the daemon's `cwd`.
    pub workspace_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum Command {
    Initialize(Initialization),
    UserMessage {
        text: String,
    },
    Cancel {
        request_id: Option<RequestId>,
    },
    ApproveToolCall {
        call_id: ToolCallId,
    },
    DenyToolCall {
        call_id: ToolCallId,
        reason: Option<String>,
    },
    ToolCallResult(ToolCallResult),
    /// Resumes a turn the turn-loop guard halted (`TurnEndReason::
    /// HaltedByIterationCap`/`HaltedByDoomLoop`), without composing a new
    /// user message -- `docs/issues/002-agent-iteration-cap-halts-real-
    /// work.md`'s resolution, decision 3 ("Continue is one action"). The
    /// session loop (`providers::rig::session::run_session_loop`) resets
    /// the guard and re-enters the turn loop from the halted result it
    /// already recorded. A safe no-op when there is nothing halted to
    /// resume (e.g. sent to an idle session, or replayed from a persisted
    /// log -- replay must never auto-resume a halted turn, so nothing in
    /// bootstrap ever sends this on a session's behalf).
    ContinueTurn,
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Initialization {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
    pub role_id: Option<RoleId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum Event {
    StateChanged(SessionState),
    ReasoningDelta(MessageDelta),
    AssistantTextDelta(MessageDelta),
    MessageCommitted(Message),
    ToolCallRequested(ToolCallRequest),
    ToolCallStarted(ToolCallId),
    ToolCallFinished(ToolCallResult),
    ApprovalRequested(ApprovalRequest),
    /// A turn's completion request left Horizon for the provider (e.g. the
    /// rig OpenAI streaming call in `providers::rig::completion`). Marks the
    /// start of the "waiting on the model" window so persisted history can
    /// attribute silence between a user message and the first delta to
    /// provider latency rather than local processing — see
    /// `docs/agent-duckdb-state-design.md`. Carries the model id so replay
    /// doesn't need to cross-reference config.
    ProviderRequestSent(ProviderRequestSent),
    /// The first chunk of any kind (text, reasoning, tool-call delta, or an
    /// error frame) arrived from the provider for the request marked by the
    /// most recent [`Event::ProviderRequestSent`]. Ends the "waiting on the
    /// model" window; the gap between the two is provider time-to-first-byte.
    ProviderRequestFirstToken,
    /// The provider's response stream for the most recent
    /// [`Event::ProviderRequestSent`] ended (normally or via cancellation).
    /// Emitted before any resulting `MessageCommitted`/`ToolCallRequested`
    /// events, so replay can bound the request's total wall-clock span.
    ProviderRequestFinished,
    Error(Error),
    Exited(Exit),
    /// A turn's explicit end, carrying why it ended — added in
    /// `docs/agent-runtime-split-design.md`'s step 4 ("Turn end becomes an
    /// explicit contract event") so bootstrap/replay never has to infer a
    /// turn's fate from state churn, and so an ACP `session/prompt`
    /// response's stop reason is derivable rather than inferred (guardrail
    /// 3). Emitted by the session loop right before the `StateChanged` that
    /// follows a turn's end (see `providers::rig::session`), so it still
    /// carries the ending turn's `turn_id` under `persistence::event_log`'s
    /// existing tracking. Folded into an `AgentFrameItem::TurnEnded` receipt
    /// by `frame::apply_agent_event_to_frame` (see
    /// `docs/agent-output-ui-amendment.md`'s 2026-07-12 addendum) — the
    /// model id and elapsed duration attached to that item are derived at
    /// fold time (from the turn's most recent `ProviderRequestSent` and a
    /// reducer-side wall clock, respectively), not carried on this event
    /// itself, so this variant's own wire shape stays unchanged.
    TurnEnded(TurnEndReason),
}

/// Why a turn ended — see [`Event::TurnEnded`]. Named after the design doc's
/// four stop reasons verbatim: "completed / cancelled / failed /
/// halted-by-guard" -- `halted-by-guard` is now the two specific
/// guard-sourced variants below rather than one bare `Halted`
/// (`docs/issues/002-agent-iteration-cap-halts-real-work.md`'s resolution,
/// decision 2): the UI needs to know *which* guard fired to render the
/// right calm reason text ("paused after 100 consecutive tool-driven
/// turns" vs. "...5 consecutive identical tool results"), and since the
/// guard's thresholds are now fixed built-in constants
/// (`config::DEFAULT_ITERATION_CAP`/`DEFAULT_DOOM_LOOP_WINDOW`) rather than
/// per-session config, the variant alone is enough for the UI to build
/// that text without carrying a number on the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum TurnEndReason {
    Completed,
    Cancelled,
    Failed,
    /// Legacy: every guard halt used this bare variant before the above
    /// resolution. Kept only so a pre-existing persisted event log with
    /// this reason still deserializes on replay; no current code path
    /// produces it. Renders the same calm "paused" treatment as the two
    /// variants below, just without a specific guard-kind sentence.
    Halted,
    /// The turn-loop guard's consecutive-tool-turn safety net stopped the
    /// turn (`providers::rig::session`'s `TurnLoopGuard::record_tool_turn`)
    /// -- see `docs/agent-tools-design.md`'s "Error Model and Loop Guards".
    HaltedByIterationCap,
    /// The turn-loop guard's doom-loop (identical-consecutive-tool-result)
    /// detector stopped the turn (`TurnLoopGuard::record_fingerprint`).
    /// Same section of the design doc.
    HaltedByDoomLoop,
}

pub fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::StateChanged(_) => "state_changed",
        Event::ReasoningDelta(_) => "reasoning_delta",
        Event::AssistantTextDelta(_) => "assistant_text_delta",
        Event::MessageCommitted(_) => "message_committed",
        Event::ToolCallRequested(_) => "tool_call_requested",
        Event::ToolCallStarted(_) => "tool_call_started",
        Event::ToolCallFinished(_) => "tool_call_finished",
        Event::ApprovalRequested(_) => "approval_requested",
        Event::ProviderRequestSent(_) => "provider_request_sent",
        Event::ProviderRequestFirstToken => "provider_request_first_token",
        Event::ProviderRequestFinished => "provider_request_finished",
        Event::Error(_) => "error",
        Event::Exited(_) => "exited",
        Event::TurnEnded(_) => "turn_ended",
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ProviderEvent {
    pub event: Event,
    pub provider_payload: Option<serde_json::Value>,
    /// Ephemeral tool-call-argument-streaming progress (see
    /// [`ToolCallProgress`]), set only via
    /// [`ProviderEvent::tool_call_progress`]. `event` is an unused
    /// placeholder whenever this is `Some`: `agent::live::State`'s reducer
    /// folds this field straight into the frame and never reads `event` for
    /// it, and `agent::live::LiveState::extend_provider_events` excludes it
    /// from the persisted event log before it reaches `Appender`. Piggy-
    /// backing on the existing `ProviderEvent` struct (rather than adding a
    /// new `Event` variant) means this "kind of event" never has to touch
    /// the event log's exhaustive `Event` matches in
    /// `persistence::projection::duckdb`.
    pub tool_call_progress: Option<ToolCallProgress>,
    /// The session's resolved model id, set only via
    /// [`ProviderEvent::session_model`] -- the session-start counterpart to
    /// `tool_call_progress` above: `event` is an unused placeholder whenever
    /// this is `Some`, it's folded as sidecar state rather than a frame item
    /// (`live::State::session_model`), and it's excluded from the persisted
    /// event log the same way (see `LiveState::extend_provider_events`).
    /// Sent once, session-scoped, by `horizon-sessiond` at session start or
    /// (re)attach (`wire::Control::SessionModel`) -- see
    /// `docs/agent-output-ui-amendment.md`'s dated model-chip addendum.
    pub session_model: Option<String>,
}

/// Tool-call-argument-streaming progress observed mid-turn, before the
/// provider's tool call is complete (rig's
/// `StreamedAssistantContent::ToolCallDelta`). Purely a UI feedback signal:
/// never folded into conversation history and never persisted — see
/// [`ProviderEvent::tool_call_progress`].
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ToolCallProgress {
    /// Rig's `internal_call_id`: stable across every delta for one tool
    /// call from the very first chunk, unlike the provider's own tool-call
    /// id which may not be known yet. Used only to fold repeated deltas for
    /// the same call into a single frame item — this is not the eventual
    /// `ToolCallId` the eventual `ToolCallRequested` carries.
    pub key: String,
    /// The tool/function name, once a `ToolCallDeltaContent::Name` chunk
    /// has been observed for this call.
    pub tool_id: Option<String>,
    /// Cumulative argument bytes streamed so far for this call.
    pub bytes: usize,
}

impl ProviderEvent {
    pub fn new(event: Event) -> Self {
        Self {
            event,
            provider_payload: None,
            tool_call_progress: None,
            session_model: None,
        }
    }

    pub fn with_provider_payload(event: Event, provider_payload: serde_json::Value) -> Self {
        Self {
            event,
            provider_payload: Some(provider_payload),
            tool_call_progress: None,
            session_model: None,
        }
    }

    /// Wraps ephemeral tool-call progress for delivery over the same
    /// `Sender<ProviderEvent>` used for real provider events
    /// (`SessionHandle::events`) — see [`ToolCallProgress`] for why `event`
    /// here is an unused placeholder rather than a new `Event` variant.
    pub fn tool_call_progress(progress: ToolCallProgress) -> Self {
        Self {
            event: Event::StateChanged(SessionState::Running),
            provider_payload: None,
            tool_call_progress: Some(progress),
            session_model: None,
        }
    }

    /// Wraps a session's resolved model id for delivery over the same
    /// channel -- see [`Self::session_model`]'s field doc comment. `event`
    /// is the same unused placeholder [`Self::tool_call_progress`] uses.
    pub fn session_model(model: String) -> Self {
        Self {
            event: Event::StateChanged(SessionState::Running),
            provider_payload: None,
            tool_call_progress: None,
            session_model: Some(model),
        }
    }
}

impl From<Event> for ProviderEvent {
    fn from(event: Event) -> Self {
        Self::new(event)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum SessionState {
    Created,
    Running,
    WaitingForUser,
    WaitingForApproval,
    ToolRunning,
    Cancelled,
    Completed,
    Failed,
    Terminated,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Message {
    pub role: MessageRole,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct MessageDelta {
    pub role: MessageRole,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ToolCallRequest {
    pub call_id: ToolCallId,
    pub tool_id: String,
    pub input: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ToolCallResult {
    pub call_id: ToolCallId,
    pub output: serde_json::Value,
    /// Explicit success/failure outcome, lifted out of `output`'s
    /// `"is_error"` JSON convention (every tool in `tools::` already writes
    /// it on failure -- `docs/agent-feedback-design.md`'s decision 1;
    /// `persistence::projection::duckdb`'s `insert_tool_result` already
    /// reads that same convention independently) so a consumer like the
    /// turn-receipts UI (`docs/agent-output-ui-amendment.md`'s 2026-07-12
    /// addendum) has a typed field instead of having to sniff `output`
    /// itself. Use [`Self::new`] rather than a struct literal to keep this
    /// derived automatically. `#[serde(default)]` (false, i.e. success) so
    /// a `Record` written before this field existed still deserializes --
    /// matching the same convention's "absence means success" reading.
    #[serde(default)]
    pub is_error: bool,
    /// Explicit marker for a user's tool-call denial, set only by
    /// [`Self::denied`] (used by `tools::approval::synchronous_result`'s
    /// `ran = false` path -- the deny arms of `resolve_synchronous_tool`/
    /// `resolve_bash` in `crates/horizon-agent/src/tools/approval.rs`).
    /// Replaces the old convention of a consumer sniffing `output` for
    /// `denied_output`'s exact `{"is_error": true, "message": "denied by
    /// user"}` shape -- documented as brittle when that convention shipped
    /// (`docs/agent-output-ui-amendment.md`'s round 3 note) since it
    /// couldn't distinguish "the field happens to read that way" from "this
    /// is contractually a denial". `#[serde(default)]` (false) so a
    /// `Record` persisted before this field existed still deserializes --
    /// `src/agent/turns.rs`'s `is_denied` falls back to the old message-text
    /// check specifically to keep classifying those old records correctly.
    #[serde(default)]
    pub denied: bool,
}

impl ToolCallResult {
    /// Builds a result with `is_error` derived from `output`'s `"is_error"`
    /// convention -- see the field's own doc comment. The single
    /// constructor every production call site should go through, so the
    /// convention lives in one place rather than being re-checked (or
    /// forgotten) at each tool.
    pub fn new(call_id: ToolCallId, output: serde_json::Value) -> Self {
        let is_error = output
            .get("is_error")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        Self {
            call_id,
            output,
            is_error,
            denied: false,
        }
    }

    /// Builds a result for a user's tool-call denial -- see the `denied`
    /// field's own doc comment. Always `is_error: true` (a denial is
    /// definitionally a failure), regardless of what `output` itself
    /// carries.
    pub fn denied(call_id: ToolCallId, output: serde_json::Value) -> Self {
        Self {
            denied: true,
            ..Self::new(call_id, output)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ApprovalRequest {
    pub call_id: ToolCallId,
    pub reason: String,
    /// Which kind of approval this is -- see [`ApprovalKind`]. `#[serde(
    /// default)]` so a `Record` persisted before this field existed still
    /// deserializes, reading as the same [`ApprovalKind::Standard`] every
    /// approval request was before this leg.
    #[serde(default)]
    pub kind: ApprovalKind,
}

/// Distinguishes the shape of a pending [`ApprovalRequest`] -- what
/// `tools::approval::resolve_bash` needs to tell an ordinary approval, a
/// sandbox-denial retry, and a network-domain-denial retry apart, since the
/// three resolve an Approve decision differently (`docs/agent-approval-
/// design.md`'s "Denial UX" and leg 4b's "denial -> approval -> retry
/// flow"). Also lets the UI render each kind with its own copy without
/// having to sniff `ApprovalRequest::reason`'s free text (today it doesn't,
/// but this keeps that door open).
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub enum ApprovalKind {
    /// An ordinary first-time approval request -- the only kind that
    /// existed before this leg.
    #[default]
    Standard,
    /// A tier-1 sandboxed `bash` call looked denied by the sandbox itself
    /// (`horizon_sandbox::is_likely_sandbox_denied`) -- approving reruns the
    /// same call with the sandbox off (`bash::BashCompletion::
    /// RetryWithoutSandbox`, `docs/agent-approval-design.md`'s "Denial UX").
    SandboxDenialRetry,
    /// A tier-1 sandboxed `bash` call's network egress was refused by the
    /// allowlist proxy for one or more domains (`bash::BashCompletion::
    /// DomainDenied`, `docs/agent-approval-design.md` leg 4b). Approving
    /// adds `domains` to this session's own allowlist and reruns the SAME
    /// call, still sandboxed; denying forwards `prior_result` as-is -- the
    /// call already ran to completion (unlike `SandboxDenialRetry`, there is
    /// a genuine result, not just a denial-shaped reason string), so a deny
    /// leaves that real, already-failed-on-its-own-terms outcome as the
    /// final one rather than synthesizing a fresh "denied by user" marker.
    DomainDenialRetry {
        domains: Vec<String>,
        prior_result: ToolCallResult,
    },
}

/// Payload for [`Event::ProviderRequestSent`]: the model id the provider was
/// asked to complete against, so the persisted event log doesn't depend on
/// separately-stored config to answer "which model was this turn waiting
/// on?".
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ProviderRequestSent {
    pub model: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) enum ToolPermission {
    AutoAllowRead,
    AutoAllowUi,
    RequireApproval,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Error {
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Exit {
    pub reason: String,
}

#[derive(Clone)]
pub struct SessionHandle {
    commands: Sender<Command>,
    events: Receiver<ProviderEvent>,
}

impl SessionHandle {
    pub fn new(commands: Sender<Command>, events: Receiver<ProviderEvent>) -> Self {
        Self { commands, events }
    }

    pub fn sender(&self) -> Sender<Command> {
        self.commands.clone()
    }

    pub fn events(&self) -> Receiver<ProviderEvent> {
        self.events.clone()
    }
}

pub trait Provider: Send + Sync {
    fn provider_id(&self) -> ProviderId;
    fn start_session(&self, request: StartSession) -> SessionHandle;
    /// The model id a session with this `role_id` would run with, resolved
    /// the same way [`Self::start_session`] resolves it (role override, else
    /// the provider's own configured default) but without spinning up a
    /// session -- pure and synchronous, so a caller can learn a session's
    /// model before (or without) starting one. `None` when this provider has
    /// no meaningful single model (e.g. the mock provider) or isn't actually
    /// going to call one (the rig provider's deterministic fallback mode,
    /// used when no API key is configured -- see
    /// `providers::rig::Provider::resolved_model`'s doc comment). Used by
    /// `horizon-sessiond` to surface a session's model to the UI from
    /// session start, ahead of any turn's `Event::ProviderRequestSent` --
    /// see `docs/agent-output-ui-amendment.md`'s dated model-chip addendum.
    fn resolved_model(&self, role_id: Option<&RoleId>) -> Option<String>;
}

#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: HashMap<ProviderId, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    /// Test-only convenience: no real event-log writer exists behind this
    /// registry, so the rig provider gets an already-resolved-to-`None`
    /// [`crate::persistence::projection::duckdb::SharedDuckdbStore`]
    /// (`SharedDuckdbStore::unavailable`) -- reads through it return
    /// immediately with no history, and never block, exactly like the
    /// pre-recall behavior of a provider constructed with no DuckDB path.
    #[cfg(test)]
    pub fn builtin() -> Self {
        Self::builtin_with_config(
            AgentConfig::from_env_and_provider(None, None),
            crate::persistence::projection::duckdb::SharedDuckdbStore::unavailable(),
        )
    }

    /// `duckdb_cell` is shared with (a clone of) whatever else in the
    /// process needs the same live DuckDB projection handle once it exists
    /// (`horizon-sessiond`'s `SessiondState`, for the recall tools) -- see
    /// `persistence::projection::duckdb::SharedDuckdbStore`'s doc comment.
    /// It's threaded in here (rather than resolved internally) because this
    /// registry -- and the rig provider it constructs -- is built at
    /// process startup, before the event log's writer thread (and
    /// therefore any real DuckDB store) exists yet.
    pub fn builtin_with_config(
        config: AgentConfig,
        duckdb_cell: crate::persistence::projection::duckdb::SharedDuckdbStore,
    ) -> Self {
        let mut registry = Self::default();
        registry.insert(Arc::new(crate::providers::mock::MockProvider::new()));
        registry.insert(Arc::new(crate::providers::rig::Provider::new(
            config.rig,
            duckdb_cell,
        )));
        registry
    }

    pub fn insert(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.provider_id(), provider);
    }

    pub fn default_provider_id(&self) -> ProviderId {
        ProviderId("builtin.agent.rig".to_string())
    }

    /// Starts a session, forwarding `role_id` to whichever provider is
    /// registered under `provider_id`. Validates `role_id` *before*
    /// dispatching to the provider -- an unresolvable role id returns
    /// `None` here exactly like an unknown `provider_id` does, so a caller
    /// that already treats `None` as "fail loudly, don't start a role-less
    /// session instead" (see `roles`'s module doc; `horizon-sessiond`'s
    /// `session::run_session` is the one production caller) gets that
    /// behavior for both failure modes without extra plumbing. This is the
    /// single choke point every session start goes through, so a role is
    /// validated the same way regardless of which provider ends up running
    /// it -- including the mock provider, which otherwise accepts and
    /// ignores `role_id` entirely (see `providers::mock`).
    pub fn start_session(
        &self,
        provider_id: &ProviderId,
        session_id: SessionId,
        role_id: Option<RoleId>,
        workspace_root: Option<PathBuf>,
    ) -> Option<SessionHandle> {
        if let Some(role_id) = &role_id {
            crate::roles::resolve(role_id)?;
        }
        self.providers.get(provider_id).map(|provider| {
            provider.start_session(StartSession {
                session_id,
                provider_id: provider_id.clone(),
                role_id,
                workspace_root,
            })
        })
    }

    /// Delegates to the named provider's [`Provider::resolved_model`].
    /// `None` for an unknown `provider_id` too -- same "nothing to report"
    /// shape as an unresolvable model, since the caller
    /// (`horizon-sessiond`'s session spawn) already handles an unknown
    /// provider as a hard session-start failure separately (see
    /// [`Self::start_session`]).
    pub fn resolved_model(
        &self,
        provider_id: &ProviderId,
        role_id: Option<&RoleId>,
    ) -> Option<String> {
        self.providers.get(provider_id)?.resolved_model(role_id)
    }
}
