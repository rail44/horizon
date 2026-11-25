//! One attachment's translation of a session's `AgentWireEvent` stream into
//! ACP v2 `session/update`s and `_horizon/*` notifications
//! (`docs/acp-agentd-implementation-plan.md` "agentd 側").
//!
//! The translation is a pure fold over the event sequence: the same
//! sequence always yields the same output, so a bootstrap replay reproduces
//! exactly what a live attachment saw. Message ids come from a per-session
//! counter that advances on every `MessageCommitted` and every
//! `ToolCallRequested`: `msg-{n}` names the user or assistant message being
//! committed (and the assistant chunks streaming ahead of it), `thought-{n}`
//! the reasoning streamed alongside it.
//!
//! Assistant chunks keep the id their stream opened with until the
//! assistant message is committed, across any tool call in between, so the
//! commit lands on the streamed message. A user or task-notification
//! message, a new provider request, a turn end, an error, or an exit closes
//! a stream that was never committed. This mirrors how
//! `horizon_agent::frame` promotes streamed deltas into the commit.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::v2::schema as v2;
use horizon_acp as acp;
use horizon_agent::contract::{
    self, ApprovalRequest, Event, MessageRole, SessionId, SessionState, TurnEndReason,
};
use horizon_agent::hosting::{AgentWireEvent, ModelSelection, ReasoningSelection};
use horizon_agent::persistence::event_log::Settled;

use crate::session::ModelGroup;

/// Everything one mapped event can put on the connection.
#[derive(Debug)]
pub(super) enum Outgoing {
    /// (Boxed: an update is several times the size of anything else here.)
    Update(Box<v2::SessionUpdate>),
    SessionEvent(acp::SessionEventNotification),
    /// An approval the client must be asked for with
    /// `session/request_permission`.
    AskPermission(Ask),
    /// The approval for this occurrence no longer waits on the client.
    ApprovalSettled(String),
}

/// An approval to ask the client for: this session's own, or one of its
/// `task` children's (`task`), shown through it and answered to the child.
#[derive(Clone, Debug)]
pub(super) struct Ask {
    pub(super) request: ApprovalRequest,
    pub(super) task: Option<SessionId>,
}

impl Outgoing {
    fn update(update: v2::SessionUpdate) -> Self {
        Outgoing::Update(Box::new(update))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StateKind {
    Running,
    Idle,
    RequiresAction,
}

/// The session facts `SessionInfoMeta` carries besides the workspace root.
#[derive(Clone, Debug)]
pub(super) struct SessionFacts {
    pub(super) provider_id: String,
    pub(super) role_id: Option<String>,
    /// The roles a session can start with, as `(id, title)`.
    pub(super) modes: Vec<(String, String)>,
    pub(super) parent_session_id: Option<SessionId>,
    pub(super) workspace_root: Option<PathBuf>,
}

pub(super) struct Mapper {
    session_id: SessionId,
    facts: SessionFacts,
    model: Option<String>,
    selection: Option<ModelSelection>,
    reasoning: Option<ReasoningSelection>,
    /// What the `model` option offers besides the current model.
    catalog: Arc<Vec<ModelGroup>>,
    message_seq: u64,
    /// The id of the assistant text stream not yet committed.
    open_stream: Option<v2::MessageId>,
    tool_ids: HashMap<String, String>,
    stop_reason: Option<v2::StopReason>,
    /// Errors of the turn in flight, not shown yet: the turn's failure
    /// carries them, or they become notices when it ends otherwise.
    turn_errors: Vec<String>,
    state: Option<StateKind>,
    pending_approvals: Vec<Ask>,
    /// While set, approvals are only recorded; the bootstrap asks for the
    /// still-pending ones after it has streamed.
    replaying: bool,
    /// Whether the position after the events mapped so far is one the
    /// conversation can branch from.
    settled: Settled,
    /// A settled position came after the last user message: the next one
    /// gets a cursor before it.
    cursor_pending: bool,
    /// The branch the session's leaf last moved to (`Event::Rewound`).
    branch: Option<String>,
}

/// The cursor before the user message `message`: a settled position the
/// session can be forked at.
fn cursor_before(message: &v2::MessageId) -> v2::SessionUpdate {
    v2::SessionUpdate::CursorUpdate(
        v2::CursorUpdate::new(cursor_id_before(message))
            .anchor(v2::CursorAnchor::UserMessage(v2::CursorMessageAnchor::new(
                message.clone(),
                v2::CursorAnchorPosition::Before,
            )))
            .capabilities(v2::CursorCapabilities::new().fork(v2::CursorForkCapabilities::new())),
    )
}

fn cursor_id_before(message: &v2::MessageId) -> v2::CursorId {
    v2::CursorId::new(format!("before-{}", message.0))
}

/// Where a fork of a session with `events` as its history ends the new
/// session's history: the index of its last event, or `None` for none.
/// At `cursor`, that is the last settled position before the user message
/// the cursor was announced before; without one, the whole history, which
/// must end settled. `Err` says why the position cannot be forked at.
pub(super) fn fork_point(
    events: &[Event],
    cursor: Option<&v2::CursorId>,
) -> Result<Option<usize>, String> {
    let mut mapper = Mapper::new(
        SessionId::new(),
        SessionFacts {
            provider_id: String::new(),
            role_id: None,
            modes: Vec::new(),
            parent_session_id: None,
            workspace_root: None,
        },
    );
    let mut settled = Settled::new();
    let mut last_settled = None;
    for (index, event) in events.iter().enumerate() {
        if let Some(cursor) = cursor {
            let announced = mapper
                .map(&AgentWireEvent::Event(event.clone()))
                .into_iter()
                .any(|outgoing| {
                    matches!(outgoing, Outgoing::Update(update)
                        if matches!(&*update, v2::SessionUpdate::CursorUpdate(announced)
                            if &announced.cursor_id == cursor))
                });
            if announced {
                return Ok(last_settled);
            }
        }
        settled.apply(event);
        if settled.is_settled() {
            last_settled = Some(index);
        }
    }
    match cursor {
        Some(cursor) => Err(format!("No cursor {} in this session", cursor.0)),
        None if settled.is_settled() => Ok(events.len().checked_sub(1)),
        None => Err("The session is in the middle of a turn; fork it at a cursor".into()),
    }
}

pub(super) fn acp_session_id(session_id: SessionId) -> v2::SessionId {
    v2::SessionId::new(session_id.as_uuid().to_string())
}

pub(super) fn parse_session_id(id: &v2::SessionId) -> Option<SessionId> {
    uuid::Uuid::parse_str(&id.0).ok().map(SessionId::from_uuid)
}

/// One model of a provider, as an entry of the `model` option.
fn model_entry(provider: &str, model: &str, name: String) -> v2::SessionConfigSelectOption {
    v2::SessionConfigSelectOption::new(acp::encode_model_option_id(provider, model), name)
}

/// The `model` config option at `(provider, model)`: the catalog's models,
/// a group for each provider, with the current one always among them. With
/// no catalog, the current model alone.
pub(super) fn model_option(
    catalog: &[ModelGroup],
    provider: &str,
    model: &str,
) -> v2::SessionConfigOption {
    let value = acp::encode_model_option_id(provider, model);
    let option = if catalog.is_empty() {
        let entry = model_entry(provider, model, format!("{provider} · {model}"));
        v2::SessionConfigOption::select(acp::MODEL_CONFIG_ID, "Model", value, vec![entry])
    } else {
        let mut groups: Vec<v2::SessionConfigSelectGroup> = (catalog.iter())
            .map(|group| {
                let mut entries: Vec<_> = (group.models.iter())
                    .map(|listed| model_entry(&group.provider, listed, listed.clone()))
                    .collect();
                if group.provider == provider && !group.models.iter().any(|m| m == model) {
                    entries.push(model_entry(provider, model, model.to_string()));
                }
                v2::SessionConfigSelectGroup::new(
                    group.provider.as_str(),
                    group.provider.as_str(),
                    entries,
                )
            })
            .collect();
        if !catalog.iter().any(|group| group.provider == provider) {
            groups.push(v2::SessionConfigSelectGroup::new(
                provider,
                provider,
                vec![model_entry(provider, model, model.to_string())],
            ));
        }
        v2::SessionConfigOption::select(acp::MODEL_CONFIG_ID, "Model", value, groups)
    };
    option.category(v2::SessionConfigOptionCategory::Model)
}

/// The `mode` config option: no role or one a session can start with, with
/// `role` current and among them.
pub(super) fn mode_option(
    modes: &[(String, String)],
    role: Option<&str>,
) -> v2::SessionConfigOption {
    let current = role.unwrap_or(acp::DEFAULT_MODE);
    let mut entries = vec![v2::SessionConfigSelectOption::new(
        acp::DEFAULT_MODE,
        "Agent",
    )];
    entries.extend(
        (modes.iter())
            .map(|(id, title)| v2::SessionConfigSelectOption::new(id.as_str(), title.as_str())),
    );
    if !entries.iter().any(|entry| &*entry.value.0 == current) {
        entries.push(v2::SessionConfigSelectOption::new(current, current));
    }
    v2::SessionConfigOption::select(acp::MODE_CONFIG_ID, "Mode", current, entries)
        .category(v2::SessionConfigOptionCategory::Mode)
}

/// The `effort` config option for a session's reasoning: its current level
/// among the levels the model takes. `None` when the model takes none.
pub(super) fn effort_option(reasoning: &ReasoningSelection) -> Option<v2::SessionConfigOption> {
    let level = reasoning.level?;
    let options = (reasoning.choices.iter())
        .map(|choice| v2::SessionConfigSelectOption::new(choice.as_str(), choice.as_str()))
        .collect::<Vec<_>>();
    Some(
        v2::SessionConfigOption::select(acp::EFFORT_CONFIG_ID, "Effort", level.as_str(), options)
            .category(v2::SessionConfigOptionCategory::ThoughtLevel),
    )
}

/// The config options for a session's applied model and reasoning: the
/// model's selection vocabulary when there is one, the provider id and
/// resolved model otherwise, nothing before a model is known; then the
/// effort, when the session's model takes one.
pub(super) fn model_options(
    catalog: &[ModelGroup],
    provider_id: &str,
    model: Option<&str>,
    selection: Option<&ModelSelection>,
    reasoning: Option<&ReasoningSelection>,
) -> Vec<v2::SessionConfigOption> {
    let mut options = match (selection, model) {
        (Some(selection), _) => vec![model_option(catalog, &selection.provider, &selection.model)],
        (None, Some(model)) => vec![model_option(catalog, provider_id, model)],
        (None, None) => Vec::new(),
    };
    options.extend(reasoning.and_then(effort_option));
    options
}

pub(super) fn horizon_meta<T: serde::Serialize>(value: &T) -> Option<v2::Meta> {
    let mut meta = None;
    acp::write_horizon_meta(&mut meta, value).expect("horizon meta serializes");
    meta
}

impl Mapper {
    pub(super) fn new(session_id: SessionId, facts: SessionFacts) -> Self {
        Self {
            session_id,
            facts,
            model: None,
            selection: None,
            reasoning: None,
            catalog: Arc::default(),
            message_seq: 0,
            open_stream: None,
            tool_ids: HashMap::new(),
            stop_reason: None,
            turn_errors: Vec::new(),
            state: None,
            pending_approvals: Vec::new(),
            replaying: true,
            settled: Settled::new(),
            cursor_pending: true,
            branch: None,
        }
    }

    pub(super) fn config_options(&self) -> Vec<v2::SessionConfigOption> {
        let mode = mode_option(&self.facts.modes, self.facts.role_id.as_deref());
        std::iter::once(mode)
            .chain(model_options(
                &self.catalog,
                &self.facts.provider_id,
                self.model.as_deref(),
                self.selection.as_ref(),
                self.reasoning.as_ref(),
            ))
            .collect()
    }

    /// Takes a new model catalog in. Whether the options it offers changed.
    pub(super) fn set_catalog(&mut self, catalog: Arc<Vec<ModelGroup>>) -> bool {
        let changed = self.catalog != catalog;
        self.catalog = catalog;
        changed
    }

    pub(super) fn config_option_update(&self) -> Option<Outgoing> {
        let options = self.config_options();
        (!options.is_empty()).then(|| {
            Outgoing::update(v2::SessionUpdate::ConfigOptionUpdate(
                v2::ConfigOptionUpdate::new(options),
            ))
        })
    }

    /// Says the session's information changed: its working directory, for
    /// one, which the client reads again from `session/list`.
    pub(super) fn session_info_update(&self) -> Outgoing {
        Outgoing::update(v2::SessionUpdate::SessionInfoUpdate(
            v2::SessionInfoUpdate::new(),
        ))
    }

    /// Folds bootstrap metadata into the mapper's state without emitting
    /// anything; the bootstrap emits the combined result itself.
    pub(super) fn absorb_metadata(&mut self, event: &AgentWireEvent) {
        match event {
            AgentWireEvent::SessionModel(model) => self.model = Some(model.clone()),
            AgentWireEvent::SessionSelection(selection) => self.selection = Some(selection.clone()),
            AgentWireEvent::SessionReasoning(reasoning) => self.reasoning = Some(reasoning.clone()),
            AgentWireEvent::WorkspaceRootResolved(resolved) => {
                self.facts.workspace_root = Some(resolved.workspace_root.clone());
                self.facts.parent_session_id = resolved.parent_session_id;
            }
            _ => {}
        }
    }

    /// Ends the bootstrap: from here on approvals are asked as they arrive.
    /// Returns the approvals the replayed history left pending.
    pub(super) fn finish_replay(&mut self) -> Vec<Ask> {
        self.replaying = false;
        self.pending_approvals.clone()
    }

    pub(super) fn map(&mut self, event: &AgentWireEvent) -> Vec<Outgoing> {
        match event {
            AgentWireEvent::Event(event) => {
                let outgoing = self.map_event(event);
                self.settled.apply(event);
                self.cursor_pending |= self.settled.is_settled();
                outgoing
            }
            AgentWireEvent::TaskProgress(progress) => {
                vec![Outgoing::update(v2::SessionUpdate::SubagentUpdate(
                    subagent_update(progress),
                ))]
            }
            AgentWireEvent::SessionModel(_)
            | AgentWireEvent::SessionSelection(_)
            | AgentWireEvent::SessionReasoning(_) => {
                self.absorb_metadata(event);
                self.config_option_update().into_iter().collect()
            }
            AgentWireEvent::WorkspaceRootResolved(_) => {
                self.absorb_metadata(event);
                vec![self.session_info_update()]
            }
            AgentWireEvent::TaskApproval {
                task_session_id,
                request,
            } => self.ask(Ask {
                request: request.clone(),
                task: Some(*task_session_id),
            }),
            AgentWireEvent::TaskApprovalSettled { occurrence_id, .. } => {
                self.settle_approval(&occurrence_id.0)
            }
            AgentWireEvent::ReplayStarted
            | AgentWireEvent::ReplayComplete
            | AgentWireEvent::AttachmentClosed(_) => Vec::new(),
        }
    }

    fn next_message_id(&self) -> v2::MessageId {
        self.id("msg")
    }

    /// The id of the `kind` of entity at the counter: after the leaf moved,
    /// with the branch it moved to, so no id on the new branch names what
    /// one on the old branch did.
    fn id(&self, kind: &str) -> v2::MessageId {
        v2::MessageId::new(match &self.branch {
            Some(branch) => format!("{kind}-{}@{branch}", self.message_seq),
            None => format!("{kind}-{}", self.message_seq),
        })
    }

    /// Drops the open assistant stream without a commit, moving the counter
    /// past its id so no later message reuses it.
    fn close_stream(&mut self) {
        if let Some(id) = self.open_stream.take()
            && id == self.next_message_id()
        {
            self.message_seq += 1;
        }
    }

    fn map_event(&mut self, event: &Event) -> Vec<Outgoing> {
        let session_id = self.session_id;
        let session_event = |event| vec![Outgoing::SessionEvent(event)];
        match event {
            Event::MessageCommitted(message) => {
                let id = match message.role {
                    MessageRole::Assistant => self
                        .open_stream
                        .take()
                        .unwrap_or_else(|| self.next_message_id()),
                    MessageRole::User | MessageRole::TaskNotification => {
                        self.close_stream();
                        self.next_message_id()
                    }
                    MessageRole::AutoContinue => {
                        if self.open_stream == Some(self.next_message_id()) {
                            self.message_seq += 1;
                        }
                        self.next_message_id()
                    }
                };
                self.message_seq += 1;
                let mut outgoing = Vec::new();
                if message.role == MessageRole::User && std::mem::take(&mut self.cursor_pending) {
                    outgoing.push(Outgoing::update(cursor_before(&id)));
                }
                let content = vec![v2::ContentBlock::from(message.text.clone())];
                let update = match message.role {
                    MessageRole::Assistant => {
                        v2::SessionUpdate::AgentMessage(v2::AgentMessage::new(id).content(content))
                    }
                    // Whoever sent it, it is a user message.
                    MessageRole::User
                    | MessageRole::TaskNotification
                    | MessageRole::AutoContinue => {
                        v2::SessionUpdate::UserMessage(v2::UserMessage::new(id).content(content))
                    }
                };
                outgoing.push(Outgoing::update(update));
                outgoing
            }
            Event::AssistantTextDelta(delta) => {
                let id = self
                    .open_stream
                    .clone()
                    .unwrap_or_else(|| self.next_message_id());
                self.open_stream = Some(id.clone());
                vec![Outgoing::update(v2::SessionUpdate::AgentMessageChunk(
                    v2::ContentChunk::new(delta.text.clone().into(), id),
                ))]
            }
            Event::ReasoningDelta(delta) => {
                vec![Outgoing::update(v2::SessionUpdate::AgentThoughtChunk(
                    v2::ContentChunk::new(delta.text.clone().into(), self.id("thought")),
                ))]
            }
            Event::ToolCallRequested(request) => {
                self.message_seq += 1;
                self.tool_ids
                    .insert(request.occurrence_id.0.clone(), request.tool_id.clone());
                let update = v2::ToolCallUpdate::new(request.occurrence_id.0.clone())
                    .title(request.tool_id.clone())
                    .name(request.tool_id.clone())
                    .kind(tool_kind(&request.tool_id))
                    .status(v2::ToolCallStatus::Pending)
                    .raw_input(request.input.0.clone());
                vec![Outgoing::update(v2::SessionUpdate::ToolCallUpdate(update))]
            }
            Event::ToolCallStarted(identity) => {
                vec![Outgoing::update(v2::SessionUpdate::ToolCallUpdate(
                    v2::ToolCallUpdate::new(identity.occurrence_id.0.clone())
                        .status(v2::ToolCallStatus::InProgress),
                ))]
            }
            Event::ToolCallFinished(result) => {
                let occurrence = result.occurrence_id.0.clone();
                let status = match result.outcome {
                    contract::ToolOutcome::Succeeded => v2::ToolCallStatus::Completed,
                    contract::ToolOutcome::Failed | contract::ToolOutcome::Denied => {
                        v2::ToolCallStatus::Failed
                    }
                    contract::ToolOutcome::Cancelled | contract::ToolOutcome::Superseded { .. } => {
                        v2::ToolCallStatus::Cancelled
                    }
                };
                // Its verdict (`auto_approved`, `policy_tier`) is in its output.
                let update = v2::ToolCallUpdate::new(occurrence.clone())
                    .status(status)
                    .raw_output(result.output.0.clone());
                let mut out = vec![Outgoing::update(v2::SessionUpdate::ToolCallUpdate(update))];
                out.extend(self.settle_approval(&occurrence));
                out
            }
            Event::ApprovalRequested(request) => self.ask(Ask {
                request: request.clone(),
                task: None,
            }),
            Event::ApprovalResolved(resolved) => self.settle_approval(&resolved.occurrence_id.0),
            Event::TurnEnded(reason) => {
                self.close_stream();
                let errors = std::mem::take(&mut self.turn_errors);
                if *reason == TurnEndReason::Failed {
                    self.stop_reason = Some(failed(&errors));
                    Vec::new()
                } else {
                    self.stop_reason = Some(stop_reason(*reason));
                    errors
                        .iter()
                        .flat_map(|error| self.error_notice(error))
                        .collect()
                }
            }
            Event::StateChanged(state) => self.map_state(*state),
            // An error in a turn waits for how the turn ends; one outside a
            // turn is a notice now.
            Event::Error(error) => {
                self.close_stream();
                if matches!(self.state, Some(StateKind::Idle) | None) {
                    self.error_notice(&error.message)
                } else {
                    self.turn_errors.push(error.message.clone());
                    Vec::new()
                }
            }
            Event::Exited(exit) => {
                self.close_stream();
                session_event(acp::SessionEventNotification::Exited {
                    session_id,
                    reason: exit.reason.clone(),
                })
            }
            Event::Rewound { branch } => {
                self.close_stream();
                self.branch = Some(branch.clone());
                Vec::new()
            }
            // What the session now runs reaches the client as the applied
            // selection the provider announces.
            Event::ModelSelected { .. } | Event::ReasoningSelected { .. } => Vec::new(),
            // A notice is for now, not the history: a replay leaves it out.
            Event::ProviderRateLimited(limited) => {
                if self.replaying {
                    return Vec::new();
                }
                let title = match limited.status {
                    Some(status) => format!("Provider rate limited (HTTP {status})"),
                    None => "Provider rate limited".to_string(),
                };
                let description = format!(
                    "attempt {}, retrying in {}s",
                    limited.attempt,
                    limited.backoff_ms.div_ceil(1000)
                );
                vec![Outgoing::update(v2::SessionUpdate::Notice(
                    v2::Notice::new(v2::NoticeSeverity::Warning, title).description(description),
                ))]
            }
            Event::HistoryCleared(cleared) => {
                self.message_seq += 1;
                let summary = format!(
                    "{} tool outputs, {} characters reclaimed",
                    cleared.cleared_occurrence_ids.len(),
                    cleared.recovered_chars
                );
                vec![Outgoing::update(v2::SessionUpdate::CompactionUpdate(
                    v2::CompactionUpdate::new(
                        self.id("compaction").to_string(),
                        v2::CompactionStatus::Completed,
                    )
                    .summary(vec![summary.into()]),
                ))]
            }
            Event::ProviderRequestSent(_) => {
                // A retried request starts its text over.
                self.close_stream();
                Vec::new()
            }
            Event::SessionResumed
            | Event::ProviderRequestFirstToken
            | Event::ProviderRequestFinished
            | Event::InputAccepted(_)
            | Event::InputStarted(_)
            | Event::InputQueuePaused(_)
            | Event::InputOutcome(_)
            | Event::DeliveryAcknowledged(_)
            | Event::SessionInputSent { .. }
            | Event::EnvironmentReady { .. }
            | Event::EnvironmentActivated(_)
            | Event::EnvironmentActivationFailed(_)
            | Event::ContinueTurnRequested(_)
            | Event::ContinuationVerdict(_)
            | Event::ConversationRecorded(_)
            | Event::ProviderRequestUsage(_) => Vec::new(),
        }
    }

    /// Records `ask` as pending, and asks for it unless the bootstrap is
    /// still replaying (then [`Self::finish_replay`] asks).
    fn ask(&mut self, ask: Ask) -> Vec<Outgoing> {
        self.pending_approvals.push(ask.clone());
        if self.replaying {
            Vec::new()
        } else {
            vec![Outgoing::AskPermission(ask)]
        }
    }

    /// An error shown now, not kept in the history: a replay leaves it out.
    fn error_notice(&self, message: &str) -> Vec<Outgoing> {
        if self.replaying {
            return Vec::new();
        }
        vec![Outgoing::update(v2::SessionUpdate::Notice(
            v2::Notice::new(v2::NoticeSeverity::Error, message),
        ))]
    }

    fn settle_approval(&mut self, occurrence: &str) -> Vec<Outgoing> {
        let before = self.pending_approvals.len();
        self.pending_approvals
            .retain(|ask| ask.request.occurrence_id.0 != occurrence);
        if self.pending_approvals.len() == before {
            return Vec::new();
        }
        vec![Outgoing::ApprovalSettled(occurrence.to_string())]
    }

    fn map_state(&mut self, state: SessionState) -> Vec<Outgoing> {
        let (kind, update) = match state {
            SessionState::Running | SessionState::ToolRunning => (
                StateKind::Running,
                v2::StateUpdate::Running(v2::RunningStateUpdate::new()),
            ),
            SessionState::WaitingForApproval => (
                StateKind::RequiresAction,
                v2::StateUpdate::RequiresAction(v2::RequiresActionStateUpdate::new()),
            ),
            SessionState::Created => {
                self.stop_reason = None;
                (
                    StateKind::Idle,
                    v2::StateUpdate::Idle(v2::IdleStateUpdate::new()),
                )
            }
            SessionState::WaitingForUser
            | SessionState::Cancelled
            | SessionState::Failed
            | SessionState::Terminated
            | SessionState::Suspended => (
                StateKind::Idle,
                v2::StateUpdate::Idle(
                    v2::IdleStateUpdate::new().stop_reason(self.stop_reason.clone()),
                ),
            ),
        };
        if self.state == Some(kind) {
            return Vec::new();
        }
        self.state = Some(kind);
        if kind == StateKind::Idle {
            self.stop_reason = None;
        }
        vec![Outgoing::update(v2::SessionUpdate::StateUpdate(update))]
    }
}

pub(super) fn tool_kind(tool_id: &str) -> v2::ToolKind {
    match tool_id {
        "fs_read" => v2::ToolKind::Read,
        "fs_edit" | "fs_write" => v2::ToolKind::Edit,
        "bash" => v2::ToolKind::Execute,
        "web_fetch" => v2::ToolKind::Fetch,
        "web_search" | "fs_grep" | "fs_glob" => v2::ToolKind::Search,
        _ => v2::ToolKind::Other,
    }
}

/// A failed turn's stop reason, with the errors the turn ran into.
fn failed(errors: &[String]) -> v2::StopReason {
    let stop = v2::ErrorStopReason::new();
    v2::StopReason::Error(if errors.is_empty() {
        stop
    } else {
        stop.error(v2::Error::new(-32603, errors.join("\n")))
    })
}

fn stop_reason(reason: TurnEndReason) -> v2::StopReason {
    match reason {
        TurnEndReason::Completed => v2::StopReason::EndTurn,
        TurnEndReason::Cancelled => v2::StopReason::Cancelled,
        TurnEndReason::Failed => failed(&[]),
        TurnEndReason::HaltedByTokenBudget => v2::StopReason::MaxTurnRequests,
    }
}

/// A short title for an approval prompt, by what the approval grants.
pub(super) fn approval_title(kind: &contract::ApprovalKind) -> &'static str {
    match kind {
        contract::ApprovalKind::Standard => "Run tool",
        contract::ApprovalKind::DomainGrant { .. } => "Allow network access",
    }
}

/// A `task` child as the requester's subagent: its description is its
/// title, and its state is mirrored onto the association.
fn subagent_update(progress: &contract::TaskProgress) -> v2::SubagentUpdate {
    let state = match progress.state {
        contract::TaskProgressState::Running => {
            v2::StateUpdate::Running(v2::RunningStateUpdate::new())
        }
        contract::TaskProgressState::WaitingForApproval => {
            v2::StateUpdate::RequiresAction(v2::RequiresActionStateUpdate::new())
        }
        contract::TaskProgressState::Finished => v2::StateUpdate::Idle(v2::IdleStateUpdate::new()),
    };
    v2::SubagentUpdate::new(acp_session_id(progress.task_session_id))
        .title(progress.description.clone())
        .state(state)
}

#[cfg(test)]
mod tests;
