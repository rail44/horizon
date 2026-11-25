mod conversation;
pub use conversation::{ConversationInputKind, ConversationRecord};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use uuid::Uuid;

use crate::roles::RoleId;
use schemars::JsonSchema;

/// The session identifier, re-exported from its home in `horizon-wire`:
/// it is the one part of a session two runtimes must agree on, so it is
/// shared foundation rather than agent vocabulary
/// (`docs/runtime-crate-alignment-design.md` judgments 1 and 2). Kept
/// re-exported at this path because `contract::SessionId` is named
/// throughout this crate and its dependents.
pub use horizon_wire::SessionId;

mod provider_event;
pub use provider_event::ProviderEvent;

pub mod tool_output;
mod tool_result;
pub use tool_result::{ToolCallResult, ToolOutcome};

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ProviderId(pub String);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct RequestId(pub String);

#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ToolCallId(pub String);

/// Per-occurrence identity for a tool call: a string Horizon mints on its own
/// side of the boundary (the rig provider never sees it, satisfying the
/// constraint that a provider knows only its own `call_id`).
///
/// A single provider `call_id` can be reused across two distinct shapes --
///
/// * provider reuse: the rig provider (or a backend it fronts) emits a fresh
///   tool call with the same `call_id` as an earlier, fully resolved one
///   (`docs/tasks/backlog.md`'s item 42, recorded from the 2026-07-18
///   reused-call_id fix `1d86521`; the `functions.fs.edit:66` incident in
///   session 05254b6a). Without a second key, every consumer -- transcript,
///   approval, analytics -- collapses the two onto the same row.
/// * denial retry: every sandbox-denial retry deliberately reissues
///   `ToolCallRequested` under the same `call_id`, so one conceptual bash
///   call renders as two transcript rows, the first stuck forever as
///   started-but-never-finished (a cosmetic defect the user sees daily).
///
/// `OccurrenceId` is the second key. It is stamped on every
/// `ToolCallRequested`, `ToolCallResult`, and `ApprovalRequest` that flows
/// out of Horizon, including start and cancellation notifications. Published
/// events require this identity; old logs must be converted before replay.
///
/// The string itself is a UUID v4 minted at the first emission point in
/// `providers::rig::mapping::rig_tool_call_request` (and at
/// `horizon_agentd::session::approval::begin_reissued_approval` for reissues) --
/// not the provider-supplied `call_id`, not a per-process counter, so a
/// resumed session and a replayed log line up on the same value without any
/// shared counter to coordinate.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct OccurrenceId(pub String);

impl OccurrenceId {
    /// Mints a fresh `OccurrenceId` (UUID v4). Use at every wire emission
    /// point that creates a new occurrence -- never at construction points
    /// that only forward an existing one.
    ///
    /// Deliberately has no `Default`, and the lint asking for one is
    /// suppressed rather than satisfied: a `Default` that silently mints a
    /// random identity is a hazard the moment any container derives
    /// `Default` and gets an occurrence nothing ever emitted.
    #[expect(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) struct StartSession {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
    /// `None` for a role-less session (unchanged behavior). `Some` must
    /// already have been validated by the caller (`ProviderRegistry::
    /// start_session`) -- see `roles`'s module doc on never silently
    /// degrading an unresolvable role id to role-less.
    pub role_id: Option<RoleId>,
    /// This session's real working directory -- for a restored session that
    /// owns a worktree, that worktree (already resolved by the caller before this
    /// reaches a provider), not wherever the daemon process happens to be
    /// running from. `None` when no root is available at all. Consumed by
    /// the rig provider to build [`crate::prompt::SessionEnvironment`]
    /// (`providers::rig::session::spawn_rig_session`), so the system
    /// prompt's "Working directory at session start" line and the skills listing
    /// (`providers::rig::session::session_extra_sections`) both reflect the
    /// session's actual root instead of the daemon's `cwd`.
    pub workspace_root: Option<PathBuf>,
    /// Prior session events for a resumed session -- empty for a fresh
    /// `Control::SessionNew`. Carries the JSONL event log's events so the
    /// rig provider can rebuild provider history from them when the DuckDB
    /// projection store is unavailable (issue 012: a resumed session must
    /// not silently lose its history when the store can't be opened). The
    /// normal path -- store available -- still loads from DuckDB; this is
    /// only the fallback when `store` is `None`.
    pub history: Vec<Event>,
    /// Whether this session's project root is in the user's `trusted_projects`
    /// config list — when `false`, repository skills (`.horizon/skills/`)
    /// and repository instructions (`AGENTS.md`/`CLAUDE.md`) are not loaded
    /// into the system prompt, and only embedded skills are advertised.
    /// Computed by `horizon-agentd` at session spawn (reusing the same
    /// `worktree::project_root` resolution as `[grants]`) and threaded in
    /// here so the rig provider's `session_extra_sections` can gate on it.
    /// `#[serde(default)]` defaults to `false` (untrusted) — fail-closed.
    #[serde(default)]
    pub trusted_project: bool,
    /// The session's modules, composed by the host; `None` has the
    /// provider compose the default ones itself. Code, not data: never
    /// serialized.
    #[serde(skip)]
    pub modules: Option<crate::modules::Modules>,
}

/// Retained Git environment identity; base is the resolved commit, never an
/// implicit fallback selected during restoration.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct SessionWorktree {
    pub repository: std::path::PathBuf,
    pub path: std::path::PathBuf,
    pub branch: String,
    pub base: String,
}

/// Identified input from a host, session, or passive notification.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct SessionInput {
    /// Explicit user instruction may resume a paused request queue. Passive
    /// notifications and autonomous session requests leave it paused.
    #[serde(default)]
    pub resume_work: bool,
    pub id: String,
    pub origin: String,
    pub text: String,
    pub reply_to: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum InputResult {
    Success { text: String },
    Failure { message: String },
    Interrupted,
}

/// The settled result, selected from a successful provider round, never from
/// an arbitrary committed transcript message. Delivery IDs survive replay.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct SessionInputOutcome {
    pub input_ids: Vec<String>,
    pub delivery_id: String,
    pub reply_to: Option<String>,
    pub outcome: InputResult,
}

/// Rebuild the durable outbox without relying on a potentially stale projection.
pub fn pending_input_outcomes(events: &[Event]) -> Vec<SessionInputOutcome> {
    let acknowledged: std::collections::HashSet<&str> = events
        .iter()
        .filter_map(|event| {
            if let Event::DeliveryAcknowledged(id) = event {
                Some(id.as_str())
            } else {
                None
            }
        })
        .collect();
    events
        .iter()
        .filter_map(|event| match event {
            Event::InputOutcome(outcome)
                if !acknowledged.contains(outcome.delivery_id.as_str()) =>
            {
                Some(outcome.clone())
            }
            _ => None,
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum Command {
    Initialize(Initialization),
    /// Durable, non-cancelling input; external addresses are opaque to providers.
    SessionInput(SessionInput),
    /// Request activation; provider stops at the next settled tool boundary.
    ActivateWorktree {
        base: String,
    },
    /// Host response to EnvironmentReady. Internal coordination command.
    EnvironmentPrepared {
        workspace_root: std::path::PathBuf,
        /// The branch the new worktree has checked out.
        branch: String,
        trusted_project: bool,
    },
    EnvironmentActivationFailed {
        message: String,
    },
    SendSessionInput {
        session_id: SessionId,
        input: SessionInput,
    },
    AcknowledgeDelivery {
        delivery_id: String,
    },
    UserMessage {
        text: String,
    },
    Cancel {
        #[serde(default)]
        request_id: Option<RequestId>,
    },
    ApproveToolCall {
        identity: ToolCallIdentity,
    },
    DenyToolCall {
        identity: ToolCallIdentity,
        #[serde(default)]
        reason: Option<String>,
    },
    ToolCallResult(ToolCallResult),
    /// Resumes a turn the turn-loop guard halted (`TurnEndReason::
    /// HaltedByTokenBudget`), without composing a new
    /// user message -- `docs/issues/002-agent-iteration-cap-halts-real-
    /// work.md`'s resolution, decision 3 ("Continue is one action"). The
    /// session loop (`providers::rig::session::SessionLoopState::run`) resets
    /// the guard and re-enters the turn loop from the halted result it
    /// already recorded. A safe no-op when there is nothing halted to
    /// resume (e.g. sent to an idle session, or replayed from a persisted
    /// log -- replay must never auto-resume a halted turn, so nothing in
    /// bootstrap ever sends this on a session's behalf).
    ContinueTurn,
    /// Request a model switch. The daemon resolves its current catalog before
    /// forwarding an internal `ApplySessionModel`. The running turn finishes
    /// on its previous selection; the provider announces only after application.
    SetSessionModel {
        provider: String,
        model: String,
    },
    /// Switch the session's reasoning effort. Like a model switch, the
    /// running turn finishes on the previous level; the provider announces
    /// the level it applied (`ProviderEvent::SessionReasoning`).
    SetSessionReasoning {
        level: crate::config::ReasoningLevel,
    },
    Shutdown,
    /// Ends the session's thread but not the session: the persisted state is
    /// [`SessionState::Suspended`] rather than `Terminated`, startup does not
    /// resume it, and an explicit resume brings it back.
    Suspend,
    /// Daemon-to-provider delivery of an already validated selection. Explicitly
    /// excluded from serialization/schema so clients cannot supply provider
    /// endpoints or key-variable names instead of selecting configured entries.
    #[serde(skip)]
    #[schemars(skip)]
    ApplySessionModel(Box<crate::config::ResolvedModelSelection>),
    /// Daemon-owned retry identity for a still-pending provider call.
    #[serde(skip)]
    #[schemars(skip)]
    ToolCallReissued(ToolCallIdentity),
    #[serde(skip)]
    #[schemars(skip)]
    ToolCallsSettled {
        id: String,
        results: Vec<ToolCallResult>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct Initialization {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
    #[serde(default)]
    pub role_id: Option<RoleId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum Event {
    InputAccepted(SessionInput),
    InputStarted(Vec<String>),
    InputQueuePaused(bool),
    EnvironmentReady {
        base: String,
    },
    EnvironmentActivated(SessionWorktree),
    EnvironmentActivationFailed(String),
    SessionResumed,
    /// The session's leaf moved: this record starts a new branch from an
    /// earlier settled position (`docs/agent-core-extension-design.md`,
    /// "二つの操作"). `branch` is new for every move, so what is announced
    /// on the new branch is never named like what was on the old one.
    Rewound {
        branch: String,
    },
    /// The session's model was switched, as asked, to `model` on the
    /// configured `provider` entry. A session started again over a history
    /// with this on its branch switches to it again.
    ModelSelected {
        provider: String,
        model: String,
    },
    /// The session's reasoning effort was set to `level`, as asked; like
    /// [`Self::ModelSelected`], kept for a session started again.
    ReasoningSelected {
        level: crate::config::ReasoningLevel,
    },

    InputOutcome(SessionInputOutcome),
    DeliveryAcknowledged(String),
    SessionInputSent {
        session_id: SessionId,
        input: SessionInput,
    },
    StateChanged(SessionState),
    ReasoningDelta(MessageDelta),
    AssistantTextDelta(MessageDelta),
    MessageCommitted(Message),
    ToolCallRequested(ToolCallRequest),
    ToolCallStarted(ToolCallIdentity),
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
    /// Exact token usage the provider reported for the most recent completed
    /// request. This is a durable inspection record, not transcript state;
    /// it is emitted separately from `ProviderRequestFinished` so providers
    /// which cannot report usage retain the existing lifecycle marker.
    ProviderRequestUsage(ProviderRequestUsage),
    /// One Tier 1 compaction pass decided which old tool results stop being
    /// sent to the provider verbatim (`docs/agent-compaction-design.md`
    /// Tier 1). This event *is* the record of that decision: the cleared set
    /// is frozen when the pass runs, so replaying this event is what makes a
    /// resumed session send the identical projection a
    /// continuously-running one would. Nothing is deleted — the full results
    /// stay in this very log and in the DuckDB projection, which is what
    /// makes the placeholder's "re-fetch via recall" pointer honest.
    HistoryCleared(HistoryCleared),
    /// A human resolved the exact pending approval occurrence. Persisted
    /// before execution or forwarding; stale and duplicate decisions produce
    /// no resolution. Replay retains it to prevent a second authorization
    /// before a provider-owned tool reports its start or finish.
    /// Judge decisions use the occurrence-tagged judge audit records instead.
    ApprovalResolved(ApprovalResolved),
    /// A human resumed a turn the turn-loop guard halted via
    /// `Command::ContinueTurn` (`docs/issues/002-agent-iteration-cap-
    /// halts-real-work.md`'s decision 3 — "Continue is one action").
    /// Emitted at the same agentd seam as `ApprovalResolved`.
    /// `resumed_from` carries the `TurnEndReason` of the most recent
    /// `TurnEnded` event in this session's frame at the moment of
    /// dispatch — `AgentFrame::last_turn_end_reason` is the accessor —
    /// so an analyst can read "human resumed after a token-budget halt"
    /// from this row alone. A
    /// `ContinueTurn` arriving for a session with nothing halted (a
    /// no-op replay, or one sent to an idle session) still emits this
    /// event with `resumed_from = None`, so analytics can count
    /// attempted-but-idle continue-turns without a separate missing-
    /// data signal: that count is itself a useful operator-behavior
    /// number (a non-zero one likely means a UI keybinding race).
    ///
    /// Audit-only: same as `ApprovalResolved`. The transcript already
    /// shows the resumed turn's `TurnEnded` receipt and the next turn's
    /// events; the audit row sits in the event log alongside them.
    ContinueTurnRequested(ContinueTurnRequested),
    /// The harness asked the decision model whether a turn the model ended
    /// with a plain reply should continue (`docs/agent-decision-model-design.md`,
    /// "The turn-continuation check"). One per check, including fallbacks
    /// and the per-turn cap. Audit-only like `ContinueTurnRequested`: a
    /// continued turn shows up as its `AutoContinue` message.
    ContinuationVerdict(ContinuationVerdict),
    /// The provider rejected a pre-generation request with a transient
    /// status (429 rate limit, 5xx gateway error, or a transport failure)
    /// and the turn is retrying after a backoff. Emitted right before the
    /// backoff sleep so the transcript shows the turn is pacing rather than
    /// stalled. Not a turn boundary — the turn is still in progress, and
    /// this item is deliberately excluded from `is_turn_boundary_item`.
    ProviderRateLimited(ProviderRateLimited),
    /// Exact provider history, independent of display and audit rows.
    ConversationRecorded(ConversationRecord),
}

/// Payload for [`Event::HistoryCleared`]: exactly which tool calls' results
/// the pass froze into the session's cleared set, and how many characters of
/// tool-result text that removed from every subsequent provider request.
///
/// `cleared_occurrence_ids` is in history order (oldest first) — the order the
/// pass walked — so a replayed set is byte-identical to the live one.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct HistoryCleared {
    pub cleared_occurrence_ids: Vec<OccurrenceId>,
    pub recovered_chars: u64,
}

/// Why a turn ended — see [`Event::TurnEnded`]: completed, cancelled,
/// failed, or paused by the turn loop's token budget
/// (`config::DEFAULT_TURN_TOKEN_BUDGET`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum TurnEndReason {
    Completed,
    Cancelled,
    Failed,
    /// The rounds since the last user input spent the turn loop's token
    /// budget (`providers::rig::guards`); `Command::ContinueTurn` resumes.
    HaltedByTokenBudget,
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
        Event::ApprovalResolved(_) => "approval_resolved",
        Event::ContinueTurnRequested(_) => "continue_turn_requested",
        Event::ContinuationVerdict(_) => "continuation_verdict",
        Event::ProviderRequestSent(_) => "provider_request_sent",
        Event::ProviderRequestFirstToken => "provider_request_first_token",
        Event::ProviderRequestFinished => "provider_request_finished",
        Event::ProviderRequestUsage(_) => "provider_request_usage",
        Event::HistoryCleared(_) => "history_cleared",
        Event::Error(_) => "error",
        Event::Exited(_) => "exited",
        Event::TurnEnded(_) => "turn_ended",
        Event::ProviderRateLimited(_) => "provider_rate_limited",
        Event::SessionInputSent { .. } => "session_input_sent",
        Event::InputAccepted(_) => "input_accepted",
        Event::InputStarted(_) => "input_started",
        Event::InputQueuePaused(_) => "input_queue_paused",
        Event::EnvironmentReady { .. } => "environment_ready",
        Event::EnvironmentActivated(_) => "environment_activated",
        Event::EnvironmentActivationFailed(_) => "environment_activation_failed",
        Event::SessionResumed => "session_resumed",
        Event::Rewound { .. } => "rewound",
        Event::ModelSelected { .. } => "model_selected",
        Event::ReasoningSelected { .. } => "reasoning_selected",
        Event::InputOutcome(_) => "input_outcome",
        Event::DeliveryAcknowledged(_) => "delivery_acknowledged",
        Event::ConversationRecorded(_) => "conversation_recorded",
    }
}

/// Live progress of one background `task` child, observed by the daemon and
/// forwarded to the requester's attached client. Purely a UI feedback signal:
/// never folded into conversation history and never persisted — see
/// [`ProviderEvent::task_progress`]. A child's completion is *also* recorded
/// durably as a `MessageRole::TaskNotification` message; the [`Self::state`]
/// transition here only retires the client's live progress row.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct TaskProgress {
    /// The child session whose progress this describes — the same id the
    /// `task` tool call's started receipt carries.
    pub task_session_id: SessionId,
    /// The child's launch description (the `task` input's `description`),
    /// carried so the client can label the row without re-reading history.
    pub description: String,
    pub state: TaskProgressState,
}

/// [`TaskProgress::state`] — a child is running, waiting for an approval
/// shown through its requester, or has reached *some* terminal state. The
/// terminal flavor (completed / failed / capped) is the notification's
/// business, not the live row's.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum TaskProgressState {
    Running,
    WaitingForApproval,
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum SessionState {
    Created,
    Running,
    WaitingForUser,
    WaitingForApproval,
    ToolRunning,
    Cancelled,
    Failed,
    Terminated,
    /// The thread ended on [`Command::Suspend`]: not running, not resumed at
    /// startup, and resumable on demand, unlike the owner's `Terminated`.
    Suspended,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct Message {
    pub role: MessageRole,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum MessageRole {
    User,
    Assistant,
    /// A system-authored notification injected into the session's own turn
    /// input -- today only the background-`task` completion notification
    /// (`docs/agent-async-task-design.md` decision 2). Deliberately *not*
    /// [`MessageRole::User`]: the provider is sent a plain user-role text
    /// message (the shape every production chat template can render), but
    /// the persisted event log must not claim a human typed it. Every
    /// consumer that distinguishes "who said this" therefore has to name
    /// this variant explicitly rather than folding it into `User`:
    /// `providers::rig::mapping::rig_messages_from_horizon_events` replays
    /// it to the provider as a user message (matching what was actually
    /// sent), while `persistence::event_log::turn::TurnTracker`,
    /// `frame`'s turn clock, and the transcript view all treat it as
    /// system-authored.
    TaskNotification,
    /// A system-authored continuation injected after the harness detected
    /// the provider truncated one or more tool calls mid-stream — the
    /// response started streaming a tool call's arguments but never
    /// finalized it, so the turn is failed and automatically continued
    /// with this message. Deliberately *not* [`MessageRole::User`]: the
    /// provider is sent a plain user-role text message (the shape every
    /// production chat template can render), but the persisted event log
    /// must not claim a human typed it. Every consumer that distinguishes
    /// "who said this" therefore has to name this variant explicitly
    /// rather than folding it into `User`:
    /// `providers::rig::mapping::rig_messages_from_horizon_events` replays
    /// it to the provider as a user message (matching what was actually
    /// sent), while `persistence::event_log::turn::TurnTracker`,
    /// `frame`'s turn clock, and the transcript view all treat it as
    /// system-authored.
    AutoContinue,
}

/// Which side of a provider conversation a [`MessageRole`] replays as.
/// Internal to `horizon-agent` -- not on the wire; see
/// [`MessageRole::provider_side`].
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProviderSide {
    User,
    Assistant,
}

impl MessageRole {
    /// Short human-readable tag for logs and the text projection
    /// (`frame::render_agent_transcript`).
    #[cfg(test)]
    pub(crate) fn log_label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::TaskNotification => "task",
            Self::AutoContinue => "continue",
            Self::Assistant => "assistant",
        }
    }

    /// The snake_case key written to DuckDB's `agent_messages.role` column
    /// and read back by `query::parse_role`. Each role is projected
    /// honestly under its own label; readers already fall back to
    /// assistant for unrecognized labels.
    pub(crate) fn db_key(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            // A system-injected background-`task` completion notification --
            // never a human turn, so it is projected as its own label
            // rather than inflating "user" message counts.
            Self::TaskNotification => "task_notification",
            // A system-injected auto-continuation after the harness detected
            // the provider truncated tool calls mid-stream -- never a human
            // turn, so it is projected as its own label.
            Self::AutoContinue => "auto_continue",
        }
    }

    /// Which side of the provider conversation this role replays as. A
    /// background-`task` notification / auto-continuation replays as a plain
    /// user-role text message because that is exactly what the provider was
    /// sent live (`providers::rig::session`'s injection): a replayed history
    /// that disagreed with the sent one would change the model's view of its
    /// own past. The distinct role exists for persistence and the transcript,
    /// not for the provider -- see [`MessageRole::TaskNotification`].
    #[cfg(test)]
    pub(crate) fn provider_side(self) -> ProviderSide {
        match self {
            Self::User | Self::TaskNotification | Self::AutoContinue => ProviderSide::User,
            Self::Assistant => ProviderSide::Assistant,
        }
    }

    /// The display label shown in the transcript view.
    pub fn display_label(self) -> &'static str {
        match self {
            Self::User => "you",
            // A background-`task` completion notification: system authored,
            // not the human's words, so it gets its own muted label rather
            // than the "you" block.
            Self::TaskNotification => "task",
            // A system-authored auto-continuation after truncation: same
            // muted treatment as a task notification.
            Self::AutoContinue => "continue",
            Self::Assistant => "agent",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct MessageDelta {
    pub role: MessageRole,
    pub text: String,
}

/// An arbitrary JSON payload on a wire vocabulary — a tool call's
/// arguments ([`ToolCallRequest::input`]) or result
/// ([`ToolCallResult::output`]), and the host-tool exchange's
/// `input`/`output`. Encoded exactly as a plain `serde_json::Value`.
///
/// `Deref`s to the inner [`serde_json::Value`] (reads like `.get(..)` and
/// indexing keep their shape); construct via `From<serde_json::Value>`,
/// unwrap via `.0`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct JsonValue(pub serde_json::Value);

impl From<serde_json::Value> for JsonValue {
    fn from(value: serde_json::Value) -> Self {
        Self(value)
    }
}

impl From<JsonValue> for serde_json::Value {
    fn from(value: JsonValue) -> Self {
        value.0
    }
}

impl std::ops::Deref for JsonValue {
    type Target = serde_json::Value;

    fn deref(&self) -> &serde_json::Value {
        &self.0
    }
}

impl std::ops::DerefMut for JsonValue {
    fn deref_mut(&mut self) -> &mut serde_json::Value {
        &mut self.0
    }
}

impl<I: serde_json::value::Index> std::ops::Index<I> for JsonValue {
    type Output = serde_json::Value;

    fn index(&self, index: I) -> &serde_json::Value {
        &self.0[index]
    }
}

impl PartialEq<serde_json::Value> for JsonValue {
    fn eq(&self, other: &serde_json::Value) -> bool {
        &self.0 == other
    }
}

impl JsonSchema for JsonValue {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "JsonValue".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "$comment": "any JSON value" })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ToolCallRequest {
    pub call_id: ToolCallId,
    pub tool_id: String,
    pub input: JsonValue,
    /// Identity of this execution attempt, minted when the request is emitted.
    pub occurrence_id: OccurrenceId,
}

/// The request identity carried through start, completion and cancellation.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ToolCallIdentity {
    pub call_id: ToolCallId,
    pub occurrence_id: OccurrenceId,
}

impl ToolCallRequest {
    pub fn identity(&self) -> ToolCallIdentity {
        ToolCallIdentity {
            call_id: self.call_id.clone(),
            occurrence_id: self.occurrence_id.clone(),
        }
    }
}

impl ApprovalRequest {
    pub fn identity(&self) -> ToolCallIdentity {
        ToolCallIdentity {
            call_id: self.call_id.clone(),
            occurrence_id: self.occurrence_id.clone(),
        }
    }
}

impl ToolCallIdentity {
    pub fn result(&self, output: impl Into<JsonValue>) -> ToolCallResult {
        ToolCallResult::new(self.call_id.clone(), self.occurrence_id.clone(), output)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ApprovalRequest {
    pub call_id: ToolCallId,
    /// Per-occurrence identity -- see [`OccurrenceId`]. Stamped by the
    /// agentd on every approval it emits (initial + every reissue) so the
    /// approval attaches to the specific occurrence the user is deciding,
    /// not just to a call_id that may have already been resolved by an
    /// earlier occurrence.
    pub occurrence_id: OccurrenceId,
    pub reason: String,
    /// Which kind of approval this is -- see [`ApprovalKind`].
    pub kind: ApprovalKind,
}

/// The operator's decision on a pending [`ApprovalRequest`], as carried on
/// [`Event::ApprovalResolved`]. Wire-stable mirror of the internal
/// `tools::approval::ApprovalDecision` enum (which is deliberately not a wire
/// type — it has no `Serialize`/`Deserialize` and lives only inside
/// `horizon-agent`), so the on-disk JSONL event log records an explicit,
/// `Deserialize`-able shape rather than depending on the internal enum's
/// representation. `Approve` carries no payload; `Deny` carries an optional
/// human-supplied reason string (the same one `Command::DenyToolCall` accepts
/// today).
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum ApprovalDecisionPayload {
    Approve,
    Deny { reason: Option<String> },
}

/// Payload for [`Event::ApprovalResolved`]. Pairs with the preceding
/// `Event::ApprovalRequested` to bound the wait-time interval an analyst
/// needs (`requested.event_at -> resolved.event_at`, both columns on
/// `agent_events`); `occurrence_id` matches the request's `occurrence_id` so
/// the join survives a provider-reused `call_id` or a sandbox-denial retry
/// (which re-mints the occurrence).
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ApprovalResolved {
    pub call_id: ToolCallId,
    pub occurrence_id: OccurrenceId,
    pub decision: ApprovalDecisionPayload,
}

/// Payload for [`Event::ContinueTurnRequested`]. Carries the `TurnEndReason`
/// of the most recent `TurnEnded` event in this session's frame at the
/// moment the human resumed, recovered via
/// [`crate::frame::AgentFrame::last_turn_end_reason`] so the analyst knows
/// which halt the operator overrode. `None` when no
/// preceding `TurnEnded` exists — the no-op-replay / idle-session case
/// (see the `Event::ContinueTurnRequested` doc comment for why that
/// distinction still matters operationally).
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ContinueTurnRequested {
    pub resumed_from: Option<TurnEndReason>,
}

/// Payload for [`Event::ContinuationVerdict`].
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ContinuationVerdict {
    /// The `next_speaker` choice (`user` or `model`); `None` when the call
    /// produced no usable answer or was not made.
    pub next_speaker: Option<String>,
    pub model_probability: Option<f64>,
    pub user_probability: Option<f64>,
    pub threshold: f64,
    pub continued: bool,
    /// Harness continuations this turn had already run when the check ran.
    pub continuations_so_far: u32,
    pub latency_ms: u64,
    /// `client_error`, `timeout`, `unparseable`, or `cap_reached`.
    pub fallback_reason: Option<String>,
    pub fallback_error: Option<String>,
}

// `Event` is `Eq`; the probabilities are never NaN (they come from a JSON
// number), so equality on them is reflexive.
impl Eq for ContinuationVerdict {}

/// What an approval decides, so `tools::approval` resolves each kind its
/// own way without reading `ApprovalRequest::reason`'s free text.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum ApprovalKind {
    /// Run the call as asked. For `bash`, once outside the sandbox with
    /// the host process's ordinary authority.
    #[default]
    Standard,
    /// Hosts beyond the session's domains. For `web_fetch`, approval adds
    /// them to the session before the request makes contact; for `bash`,
    /// they open for that one sandboxed call and nothing is added.
    DomainGrant { domains: Vec<String> },
}

impl ApprovalKind {
    /// The result of the attempt that already ran, for a kind that offers
    /// to retry one; `None` for a kind asked before anything ran.
    pub fn prior_result(&self) -> Option<&ToolCallResult> {
        match self {
            // Named rather than `_`, so that a new kind does not compile
            // until it says whether it holds a prior result.
            Self::Standard | Self::DomainGrant { .. } => None,
        }
    }
}

/// Payload for [`Event::ProviderRequestSent`]: the model id the provider was
/// asked to complete against, so the persisted event log doesn't depend on
/// separately-stored config to answer "which model was this turn waiting
/// on?".
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ProviderRequestSent {
    pub model: String,
}

/// Payload for [`Event::ProviderRateLimited`]: the provider rejected a
/// pre-generation request with a transient status and the turn is waiting
/// out a backoff before retrying. Emitted from the retry loop
/// (`providers::rig::completion::with_pre_generation_retry`) right before
/// the backoff sleep, so the transcript shows the turn is pacing against
/// the provider's rate limit rather than stalled.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ProviderRateLimited {
    /// The HTTP status that caused the retry (429, 500, 502, 503, 504), or
    /// `None` for a transport-level failure.
    pub status: Option<u16>,
    /// Which attempt this is (1-based). For 429 this climbs without bound;
    /// for 5xx and transport failures it is bounded by
    /// `PROVIDER_REQUEST_MAX_ATTEMPTS`.
    pub attempt: u32,
    /// How long the turn will wait before retrying, in milliseconds.
    pub backoff_ms: u64,
}

/// Exact usage reported by a provider for one completion request. The fields
/// use provider-neutral input/output names while retaining separately reported
/// cached input for later JSONL or DuckDB inspection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ProviderRequestUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub(crate) enum ToolPermission {
    AutoAllowRead,
    AutoAllowUi,
    RequireApproval,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct Error {
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct Exit {
    pub reason: String,
}

#[cfg(test)]
mod json_value_tests {
    use super::*;

    #[test]
    fn clients_cannot_send_internal_retry_identity_updates() {
        let identity = ToolCallIdentity {
            call_id: ToolCallId("internal-retry".into()),
            occurrence_id: OccurrenceId::new(),
        };
        assert!(
            serde_json::from_value::<Command>(serde_json::json!({
                "ToolCallReissued": identity,
            }))
            .is_err()
        );
        assert!(serde_json::to_value(Command::ToolCallReissued(identity)).is_err());
    }

    /// The load-bearing property of [`JsonValue`]'s human-readable path:
    /// under serde_json the wrapper is *transparent* — byte-identical to a
    /// plain `serde_json::Value` field — so the event log's on-disk JSONL
    /// format is unchanged by the v10 cutover and pre-v10 log lines still
    /// decode.
    #[test]
    fn json_value_is_transparent_under_serde_json() {
        let inner = serde_json::json!({"path": "a.txt", "nested": [1, 2, {"k": true}]});
        let wrapped = JsonValue::from(inner.clone());
        assert_eq!(
            serde_json::to_string(&wrapped).unwrap(),
            serde_json::to_string(&inner).unwrap(),
            "the wrapper must add no encoding of its own under JSON"
        );
        // The old on-disk shape (a raw JSON object, as a plain `Value`
        // field wrote it) decodes into the wrapper unchanged.
        let decoded: JsonValue = serde_json::from_str(&inner.to_string()).unwrap();
        assert_eq!(decoded, inner);
    }

    /// A whole `ToolCallRequest` — the shape the event log actually
    /// persists inside its records — serializes with `input` as the raw
    /// JSON object, exactly as the pre-v10 `serde_json::Value` field did.
    #[test]
    fn tool_call_request_encodes_input_as_json_with_required_identity() {
        let request = ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: "fs_read".to_string(),
            input: serde_json::json!({"path": "a.txt"}).into(),
            occurrence_id: crate::contract::OccurrenceId("call-1".to_string()),
        };
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({
                "call_id": "call-1",
                "tool_id": "fs_read",
                "input": {"path": "a.txt"},
                "occurrence_id": "call-1",
            })
        );
        let decoded: ToolCallRequest = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, request);
    }

    /// `ToolCallResult::new`'s `is_error` convention still reads through
    /// the wrapper.
    #[test]
    fn tool_call_result_new_reads_is_error_through_the_wrapper() {
        let result = ToolCallResult::new(
            ToolCallId("call-1".to_string()),
            crate::contract::OccurrenceId("call-1".to_string()),
            serde_json::json!({"is_error": true, "message": "boom"}),
        );
        assert!(result.is_error());
    }

    #[test]
    fn domain_grant_approval_round_trips_with_its_exact_hosts() {
        let request = ApprovalRequest {
            call_id: ToolCallId("fetch-1".to_string()),
            occurrence_id: crate::contract::OccurrenceId("fetch-1".to_string()),
            reason: "allow exact host".to_string(),
            kind: ApprovalKind::DomainGrant {
                domains: vec!["docs.example.com".to_string()],
            },
        };
        let encoded = serde_json::to_value(&request).unwrap();
        let decoded: ApprovalRequest = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, request);
    }
}

/// Pins the exact output of every [`MessageRole`] mapping so a refactor
/// cannot silently change a role's label, DB key, provider side, or display
/// label. Adding a `MessageRole` variant forces a new arm in each mapping's
/// non-exhaustive match (one compile cycle, all at once); this test then
/// forces the author to pin the new variant's expected output too.
#[cfg(test)]
mod message_role_tests {
    use super::*;

    #[test]
    fn log_label_is_pinned_per_variant() {
        assert_eq!(MessageRole::User.log_label(), "user");
        assert_eq!(MessageRole::Assistant.log_label(), "assistant");
        assert_eq!(MessageRole::TaskNotification.log_label(), "task");
        assert_eq!(MessageRole::AutoContinue.log_label(), "continue");
    }

    #[test]
    fn db_key_is_pinned_per_variant() {
        assert_eq!(MessageRole::User.db_key(), "user");
        assert_eq!(MessageRole::Assistant.db_key(), "assistant");
        assert_eq!(MessageRole::TaskNotification.db_key(), "task_notification");
        assert_eq!(MessageRole::AutoContinue.db_key(), "auto_continue");
    }

    #[test]
    fn provider_side_is_pinned_per_variant() {
        assert_eq!(MessageRole::User.provider_side(), ProviderSide::User);
        assert_eq!(
            MessageRole::TaskNotification.provider_side(),
            ProviderSide::User
        );
        assert_eq!(
            MessageRole::AutoContinue.provider_side(),
            ProviderSide::User
        );
        assert_eq!(
            MessageRole::Assistant.provider_side(),
            ProviderSide::Assistant
        );
    }

    #[test]
    fn display_label_is_pinned_per_variant() {
        assert_eq!(MessageRole::User.display_label(), "you");
        assert_eq!(MessageRole::Assistant.display_label(), "agent");
        assert_eq!(MessageRole::TaskNotification.display_label(), "task");
        assert_eq!(MessageRole::AutoContinue.display_label(), "continue");
    }
}
