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
/// out of Horizon. `#[serde(default)]` on the field sites means a peer (older
/// agentd, replayed pre-feature log) that never minted one decodes cleanly
/// with `None`, and the consumers fall back to call_id + position scanning
/// (`frame.rs:189-291`'s existing `.rev()` semantic), so the wire change is
/// additive and needs no `SESSION_PROTOCOL_VERSION` bump.
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
    #[allow(clippy::new_without_default)]
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
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub enum Command {
    Initialize(Initialization),
    UserMessage {
        text: String,
    },
    Cancel {
        #[serde(default)]
        request_id: Option<RequestId>,
    },
    ApproveToolCall {
        call_id: ToolCallId,
    },
    DenyToolCall {
        call_id: ToolCallId,
        #[serde(default)]
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

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct Initialization {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
    #[serde(default)]
    pub role_id: Option<RoleId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
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
    /// A human resolved a pending `ApprovalRequested`. Emitted at the
    /// agentd seam where `Command::ApproveToolCall`/`DenyToolCall` lands
    /// in `dispatch_inbound_command` (`crates/horizon-agentd/src/session/
    /// approval.rs`), *before* any `ToolCallStarted`/`ToolCallFinished` the
    /// resolution may then go on to produce — so the audit row exists
    /// regardless of which `ApprovalOutcome` variant `resolve_approval`
    /// returns, including the `AlreadyResolved` duplicate-click case.
    ///
    /// This is the **authoritative** record of who resolved a pending
    /// approval and how (the existing `agent_approvals.outcome` column
    /// stays populated by the order-derived `ToolCallStarted`/`Tool
    /// CallFinished` path for backward compatibility, but it is a derived
    /// best-effort projection — collapsed rows and reused `call_id`s can
    /// mis-stamp it; this event is what analysis reads first).
    /// `Event::ApprovalRequested` + `Event::ApprovalResolved` pair up the
    /// `requested -> resolved` interval an analyst wants for wait-time
    /// numbers; `ApprovalResolved::occurrence_id` carries the same
    /// `OccurrenceId` the matching `ApprovalRequested` was minted with.
    ///
    /// Deliberately carries only the *human* decision: judge-issued
    /// approvals (`tools::approval::resolve_auto_approval`, the enforcing
    /// judge's auto-resolve path) do not produce this event, because the
    /// whole point is to surface what the operator did, not what the
    /// background model decided. Auto-approvals are still visible via
    /// `agent_approvals.outcome` and the `judge_*` event log records
    /// (`docs/agent-approval-design.md`'s "Judge design").
    ///
    /// Audit-only: no frame item, no projection table row. The transcript
    /// already shows the resolution as the approval-row state changing
    /// (approve → `ToolCallStarted` / `ToolCallFinished`; deny →
    /// `ToolCallFinished` with `denied: true`); adding a row here would
    /// duplicate that signal for the user while doing nothing for SQL
    /// analytics that read `agent_events` directly.
    ApprovalResolved(ApprovalResolved),
    /// A human resumed a turn the turn-loop guard halted via
    /// `Command::ContinueTurn` (`docs/issues/002-agent-iteration-cap-
    /// halts-real-work.md`'s decision 3 — "Continue is one action").
    /// Emitted at the same agentd seam as `ApprovalResolved`.
    /// `resumed_from` carries the `TurnEndReason` of the most recent
    /// `TurnEnded` event in this session's frame at the moment of
    /// dispatch — `AgentFrame::last_turn_end_reason` is the accessor —
    /// so an analyst can read "human resumed after an iteration-cap
    /// halt" / "...after a doom-loop halt" from this row alone. A
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
    /// The provider rejected a pre-generation request with a transient
    /// status (429 rate limit, 5xx gateway error, or a transport failure)
    /// and the turn is retrying after a backoff. Emitted right before the
    /// backoff sleep so the transcript shows the turn is pacing rather than
    /// stalled. Not a turn boundary — the turn is still in progress, and
    /// this item is deliberately excluded from `is_turn_boundary_item`.
    ProviderRateLimited(ProviderRateLimited),
    /// One turn's incremental update to a standing agent's memory document
    /// (`docs/standing-agent-memory-design.md`). A standing-role session
    /// (one whose `RoleDefinition::standing` is true) must end every turn
    /// either by calling the `memory.update` tool — producing this event with
    /// the field-level operations applied — or by declaring no update is
    /// needed, producing this event with `no_update_reason` set. Both satisfy
    /// the turn-end checkpoint; a turn that does neither is reminded once and
    /// then closed with a `MemoryCheckpointMissed` event if it still hasn't.
    ///
    /// Like `HistoryCleared`, this is a durable record the provider-view
    /// projection consumes: a standing session's sent history is assembled as
    /// `[brief][current memory document][recent tail]`, and the document is
    /// reconstructed by replaying these events' field operations in order
    /// (`tools::memory::memory_document_from_events`). The raw turn-by-turn
    /// log stays in the event log untouched — `recall` can still reach it —
    /// which is what makes the document's "re-fetch via recall" pointer honest.
    MemoryDigest(MemoryDigest),
    /// The turn-loop checkpoint for a standing-role session closed a turn that
    /// did not update the memory document and did not declare no-update, even
    /// after one reminder was injected (`docs/standing-agent-memory-design.md`
    /// decision 2, checkpoint). Audit/transparency-only: visible in the
    /// transcript as a marker that the agent skipped its memory checkpoint,
    /// so an operator can see how often a standing model writes itself out of
    /// its own continuity — the dogfood measurement the design defers quality
    /// to. No frame item is needed beyond the marker; the turn still ends
    /// `Completed` (the checkpoint is a structural gate, not an error).
    MemoryCheckpointMissed,
    /// A standing-agent session was seeded from a prior keeper session's
    /// folded `MemoryDigest` sequence at startup (the wake-action v2
    /// seed-spawn path, board #39/#41). Emitted once, right after the init
    /// `StateChanged(WaitingForUser)`, so the event log proves the seed was
    /// applied — the startup events are otherwise identical to a fresh
    /// spawn (`Created` + `Assistant`-role init + `WaitingForUser`).
    /// Audit/observability-only: no frame item, no projection beyond the
    /// event log row.
    MemorySeeded,
}

/// Payload for [`Event::HistoryCleared`]: exactly which tool calls' results
/// the pass froze into the session's cleared set, and how many characters of
/// tool-result text that removed from every subsequent provider request.
///
/// `cleared_call_ids` is in history order (oldest first) — the order the
/// pass walked — so a replayed set is byte-identical to the live one.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct HistoryCleared {
    pub cleared_call_ids: Vec<ToolCallId>,
    pub recovered_chars: u64,
}

/// A structured field of the standing-agent memory document
/// (`docs/standing-agent-memory-design.md`). The seven fields are the Tier 2
/// compaction template verbatim — goal / decisions / completed / in-progress /
/// stuck / next step / related files and symbols — carried on the wire as
/// snake_case so they round-trip through the `memory.update` tool's JSON
/// schema unchanged. All fields are free-text blocks; the template gives the
/// structure, the agent gives the content.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryField {
    Goal,
    Decisions,
    Completed,
    InProgress,
    Stuck,
    NextStep,
    Related,
}

/// A field-level operation in a [`MemoryDigest`]. `Set` replaces the field's
/// content, `Append` adds to it, and `Clear` empties it — the three operations
/// the `memory.update` tool exposes, so the agent edits one field at a time
/// rather than regenerating the whole document (full-document regeneration
/// collapses under iteration: ACE 66.7→57.1%, codex#14589 13.7→6.9% —
/// `docs/research/standing-agent-memory-evidence-2026-08-15.md` §2-1).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MemoryOp {
    Set,
    Append,
    Clear,
}

/// A reference to the span of the raw event log the digest summarizes — so a
/// reader (the agent itself, an operator, or #35's wake-side seeding) can
/// `recall.read` the original turn-by-turn exchanges the document condenses.
/// Sequence numbers are the event log's own ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct FoldedLogRange {
    pub from_seq: u64,
    pub to_seq: u64,
}

/// One field-level update in a [`MemoryDigest`]: which field, which operation,
/// and the content (ignored when `op` is `Clear`).
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct MemoryFieldUpdate {
    pub field: MemoryField,
    pub op: MemoryOp,
    pub content: String,
}

/// Payload for [`Event::MemoryDigest`]: one turn's incremental edit to the
/// standing agent's memory document. Replaying every `MemoryDigest` event's
/// `updates` in log order reconstructs the current document exactly
/// (`tools::memory::memory_document_from_events`) — the document is never
/// carried as a whole, only as the per-turn diffs that built it, which is what
/// keeps a long-running standing session's event log small under iteration.
///
/// Exactly one of `updates` (non-empty) or `no_update_reason` is set: the
/// agent either applied field operations or declared no update was needed.
/// `folded_log_range` is present only when `updates` is non-empty.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct MemoryDigest {
    pub updates: Vec<MemoryFieldUpdate>,
    pub folded_log_range: Option<FoldedLogRange>,
    pub no_update_reason: Option<String>,
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
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
        Event::ApprovalResolved(_) => "approval_resolved",
        Event::ContinueTurnRequested(_) => "continue_turn_requested",
        Event::ProviderRequestSent(_) => "provider_request_sent",
        Event::ProviderRequestFirstToken => "provider_request_first_token",
        Event::ProviderRequestFinished => "provider_request_finished",
        Event::ProviderRequestUsage(_) => "provider_request_usage",
        Event::HistoryCleared(_) => "history_cleared",
        Event::Error(_) => "error",
        Event::Exited(_) => "exited",
        Event::TurnEnded(_) => "turn_ended",
        Event::ProviderRateLimited(_) => "provider_rate_limited",
        Event::MemoryDigest(_) => "memory_digest",
        Event::MemoryCheckpointMissed => "memory_checkpoint_missed",
        Event::MemorySeeded => "memory_seeded",
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
    /// Sent once, session-scoped, by `horizon-agentd` at session start or
    /// (re)attach (`wire::AgentWireEvent::SessionModel`) -- see
    /// `docs/agent-output-ui-amendment.md`'s dated model-chip addendum.
    pub session_model: Option<String>,
}

/// Tool-call-argument-streaming progress observed mid-turn, before the
/// provider's tool call is complete (rig's
/// `StreamedAssistantContent::ToolCallDelta`). Purely a UI feedback signal:
/// never folded into conversation history and never persisted — see
/// [`ProviderEvent::tool_call_progress`].
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ToolCallProgress {
    /// Rig's `internal_call_id`: stable across every delta for one tool
    /// call from the very first chunk, unlike the provider's own tool-call
    /// id which may not be known yet. Used only to fold repeated deltas for
    /// the same call into a single frame item — this is not the eventual
    /// `ToolCallId` the eventual `ToolCallRequested` carries.
    pub key: String,
    /// The tool/function name, once a `ToolCallDeltaContent::Name` chunk
    /// has been observed for this call.
    #[serde(default)]
    pub tool_id: Option<String>,
    /// Cumulative argument bytes streamed so far for this call.
    pub bytes: usize,
}

impl ProviderEvent {
    pub(crate) fn new(event: Event) -> Self {
        Self {
            event,
            provider_payload: None,
            tool_call_progress: None,
            session_model: None,
        }
    }

    pub(crate) fn with_provider_payload(event: Event, provider_payload: serde_json::Value) -> Self {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
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
/// `input`/`output`. Wraps `serde_json::Value` with a format-aware serde
/// encoding, keyed on serde's `is_human_readable`:
///
/// - **Human-readable formats** (serde_json — the event log's on-disk
///   JSONL, test fixtures) see the value *transparently*: the encoded
///   bytes are identical to a plain `serde_json::Value` field, so the
///   persisted event-log format is unchanged by the v10 cutover
///   (`docs/remoc-adoption-design.md` §6: on-disk format out of scope) and
///   every pre-v10 log line still decodes.
/// - **Binary formats** (the v10 Postbag wire) carry it as its JSON *text*
///   in one string. `serde_json::Value`'s own `Deserialize` is built on
///   serde's `deserialize_any`, which only a self-describing format can
///   answer — Postbag rejects it outright (`DeserializeAnyUnsupported`),
///   so a raw `Value` cannot cross the v10 wire at all. Tool I/O is
///   control-plane traffic; the double encode is an accepted cost for
///   keeping the single pinned Postbag codec (owner decision, 2026-07-20).
///
/// `Deref`s to the inner [`serde_json::Value`] (reads like `.get(..)` and
/// indexing keep their shape); construct via `From<serde_json::Value>`,
/// unwrap via `.0`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JsonValue(pub serde_json::Value);

#[cfg(test)]
impl JsonValue {
    pub(crate) fn new(value: serde_json::Value) -> Self {
        Self(value)
    }
}

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

impl Serialize for JsonValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            self.0.serialize(serializer)
        } else {
            serializer.serialize_str(&self.0.to_string())
        }
    }
}

impl<'de> Deserialize<'de> for JsonValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            serde_json::Value::deserialize(deserializer).map(Self)
        } else {
            let text = String::deserialize(deserializer)?;
            serde_json::from_str(&text)
                .map(Self)
                .map_err(serde::de::Error::custom)
        }
    }
}

impl JsonSchema for JsonValue {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "JsonValue".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Any JSON value, like `serde_json::Value`'s own schema — with the
        // wire-encoding note that distinguishes it.
        schemars::json_schema!({
            "$comment": "any JSON value; on the binary (Postbag) wire it travels as its JSON \
                         text in one string"
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ToolCallRequest {
    pub call_id: ToolCallId,
    pub tool_id: String,
    pub input: JsonValue,
    /// Per-occurrence identity -- see [`OccurrenceId`]. `None` when the
    /// originating request is not in scope (synthetic results); consumers
    /// fall back to `call_id` + positional `.rev()` scanning in that case.
    pub occurrence_id: Option<OccurrenceId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ToolCallResult {
    pub call_id: ToolCallId,
    /// Per-occurrence identity -- see [`OccurrenceId`]. Mirrors the
    /// `ToolCallRequest` field it answers to.
    pub occurrence_id: Option<OccurrenceId>,
    pub output: JsonValue,
    /// Explicit success/failure outcome, lifted out of `output`'s
    /// `"is_error"` JSON convention (every tool in `tools::` already writes
    /// it on failure -- `docs/agent-feedback-design.md`'s decision 1;
    /// `persistence::projection::duckdb`'s `insert_tool_result` already
    /// reads that same convention independently) so a consumer like the
    /// turn-receipts UI (`docs/agent-output-ui-amendment.md`'s 2026-07-12
    /// addendum) has a typed field instead of having to sniff `output`
    /// itself. Use [`Self::new`] rather than a struct literal to keep this
    /// derived automatically.
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
    /// is contractually a denial".
    pub denied: bool,
}

impl ToolCallResult {
    /// Builds a result with `is_error` derived from `output`'s `"is_error"`
    /// convention -- see the field's own doc comment. The single
    /// constructor every production call site should go through, so the
    /// convention lives in one place rather than being re-checked (or
    /// forgotten) at each tool.
    ///
    /// `occurrence_id` is the per-occurrence identity from the originating
    /// `ToolCallRequest` (see [`OccurrenceId`]); `None` is acceptable when
    /// the originating request is not in scope (replayed logs, synthetic
    /// results). `transcript::tool_call::build_tool_call_views` matches
    /// `ToolCallFinished` events back to their `Building` entry by
    /// `occurrence_id` first, falling back to call_id + position -- so a
    /// `None` here does not break the transcript, it just removes the
    /// per-occurrence attribution that an older or synthetic event
    /// doesn't carry.
    pub fn new(
        call_id: ToolCallId,
        occurrence_id: Option<OccurrenceId>,
        output: impl Into<JsonValue>,
    ) -> Self {
        let output = output.into();
        let is_error = output
            .get("is_error")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        Self {
            call_id,
            occurrence_id,
            output,
            is_error,
            denied: false,
        }
    }

    /// Builds a result for a user's tool-call denial -- see the `denied`
    /// field's own doc comment. Always `is_error: true` (a denial is
    /// definitionally a failure), regardless of what `output` itself
    /// carries. `occurrence_id` is forwarded the same way as [`Self::new`].
    pub(crate) fn denied(
        call_id: ToolCallId,
        occurrence_id: Option<OccurrenceId>,
        output: impl Into<JsonValue>,
    ) -> Self {
        Self {
            denied: true,
            ..Self::new(call_id, occurrence_id, output)
        }
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
    pub occurrence_id: Option<OccurrenceId>,
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
    pub occurrence_id: Option<OccurrenceId>,
    pub decision: ApprovalDecisionPayload,
}

/// Payload for [`Event::ContinueTurnRequested`]. Carries the `TurnEndReason`
/// of the most recent `TurnEnded` event in this session's frame at the
/// moment the human resumed, recovered via
/// [`crate::frame::AgentFrame::last_turn_end_reason`] so the analyst knows
/// which guard's halt the operator overrode (`HaltedByIterationCap` /
/// `HaltedByDoomLoop` / the legacy bare `Halted`). `None` when no
/// preceding `TurnEnded` exists — the no-op-replay / idle-session case
/// (see the `Event::ContinueTurnRequested` doc comment for why that
/// distinction still matters operationally).
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct ContinueTurnRequested {
    pub resumed_from: Option<TurnEndReason>,
}

/// Distinguishes the shape of a pending [`ApprovalRequest`] -- what
/// `tools::approval::resolve_bash` needs to tell an ordinary approval, a
/// sandbox-denial retry, and a network-domain-denial retry apart, since the
/// three resolve an Approve decision differently (`docs/agent-approval-
/// design.md`'s "Denial UX" and leg 4b's "denial -> approval -> retry
/// flow"). Also lets the UI render each kind with its own copy without
/// having to sniff `ApprovalRequest::reason`'s free text (today it doesn't,
/// but this keeps that door open).
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize, JsonSchema)]
// `large_enum_variant` triggered after adding `occurrence_id: Option<
// OccurrenceId>` to `ToolCallResult` pushed the embedded variants
// (DomainDenied, FilesystemDenied, ...) just past the 200-byte threshold
// the lint uses. Boxing the embedded `result` fields would propagate
// `Box` allocations through every match site; the variants are not
// constructed in a hot loop, and the enum is not constructed many at
// once, so the simpler shape is preferred. `clippy::large_enum_variant`
// is allow-listed here rather than at each variant -- the same enum
// sits behind `BashCompletion` too, and the alternative spelling would
// multiply without adding a real signal.
#[allow(clippy::large_enum_variant)]
pub enum ApprovalKind {
    /// An ordinary first-time approval request -- the only kind that
    /// existed before this leg.
    #[default]
    Standard,
    /// Legacy event-log compatibility for containment denials which did not
    /// name a narrow grant. New execution never emits this kind, and an old
    /// pending request fails closed instead of retrying without containment.
    SandboxDenialRetry,
    /// A tier-1 sandboxed `bash` call's network egress was refused by the
    /// allowlist proxy for one or more domains (`bash::BashCompletion::
    /// DomainDenied`, `docs/agent-approval-design.md` leg 4b). Approving
    /// adds `domains` to this session's own allowlist and reruns the SAME
    /// call, still sandboxed; denying forwards `prior_result` as-is -- the
    /// call already ran to completion, so a deny
    /// leaves that real, already-failed-on-its-own-terms outcome as the
    /// final one rather than synthesizing a fresh "denied by user" marker.
    DomainDenialRetry {
        domains: Vec<String>,
        prior_result: ToolCallResult,
    },
    /// A sandboxed `bash` call was refused access to paths outside its
    /// workspace. Approval adds `grants` to this session and reruns the
    /// SAME call **still sandboxed**; denying forwards `prior_result` as-is
    /// (the call already ran, and its output already reflects the refusal).
    ///
    /// `denials` are the raw mediated attempts -- evidence, shown to the
    /// approver as the trigger. `grants` is what approval actually buys:
    /// the shaped suggestion (`horizon_sandbox::suggest_grants`, one tree
    /// at the attempts' narrowest common ancestor, clamped), which is
    /// generally *not* the same as the per-attempt grants inside `denials`.
    ///
    /// Both the variant name and `denials` predate the 2026-07-26
    /// project-scoped-tree-grants decision
    /// (`docs/containment-denial-narrow-grants-design.md`) and stay put so
    /// existing event-log records keep deserializing. `grants` is
    /// `#[serde(default)]` for the same reason: a request persisted by the
    /// 2026-07-24 host-execution build reads back with none, and an empty
    /// grant list fails closed at approval rather than silently reviving
    /// unsandboxed execution.
    FilesystemDenialRetry {
        denials: Vec<horizon_sandbox::FilesystemDenial>,
        #[serde(default)]
        grants: Vec<horizon_sandbox::FilesystemGrant>,
        prior_result: ToolCallResult,
    },
    /// One or more Horizon-derived public domains must be added to this
    /// session before a host-side web request may contact them. Unlike
    /// `DomainDenialRetry`, no denied tool result exists yet: contact has
    /// not occurred. Approval adds only these domains and starts the same
    /// request; denial resolves it without network access.
    DomainGrant { domains: Vec<String> },
    /// A Git command that may mutate repository metadata in an isolated
    /// linked worktree. The paths were derived and validated by Horizon
    /// before this request was displayed. Approval re-resolves them, grants
    /// them only to this call (including any chained containment retry), and
    /// keeps the command inside the sandbox.
    GitOperation { writable_roots: Vec<PathBuf> },
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

    /// The load-bearing property of [`JsonValue`]'s human-readable path:
    /// under serde_json the wrapper is *transparent* — byte-identical to a
    /// plain `serde_json::Value` field — so the event log's on-disk JSONL
    /// format is unchanged by the v10 cutover and pre-v10 log lines still
    /// decode. (The binary-wire path is proven under the actual Postbag
    /// codec in this crate's `tests/skew.rs`.)
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
    fn tool_call_request_keeps_the_pre_v10_json_shape() {
        let request = ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: "fs.read".to_string(),
            input: serde_json::json!({"path": "a.txt"}).into(),
            occurrence_id: None,
        };
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({
                "call_id": "call-1",
                "tool_id": "fs.read",
                "input": {"path": "a.txt"},
                "occurrence_id": null,
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
            None,
            serde_json::json!({"is_error": true, "message": "boom"}),
        );
        assert!(result.is_error);
    }

    #[test]
    fn domain_grant_approval_round_trips_with_its_exact_hosts() {
        let request = ApprovalRequest {
            call_id: ToolCallId("fetch-1".to_string()),
            occurrence_id: None,
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
