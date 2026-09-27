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

use std::collections::HashMap;
use std::path::PathBuf;

use agent_client_protocol::schema::v2;
use horizon_acp as acp;
use horizon_agent::contract::{
    self, ApprovalRequest, Event, MessageRole, SessionId, SessionState, TurnEndReason,
};
use horizon_agent::wire::{AgentWireEvent, ModelSelection};

/// Everything one mapped event can put on the connection.
#[derive(Debug)]
pub(super) enum Outgoing {
    Update(v2::SessionUpdate),
    TaskProgress(acp::TaskProgressNotification),
    ToolCallProgress(acp::ToolCallProgressNotification),
    Memory(acp::MemoryNotification),
    SessionEvent(acp::SessionEventNotification),
    ProviderRequest(acp::ProviderRequestNotification),
    /// An approval the client must be asked for with
    /// `session/request_permission`.
    AskPermission(ApprovalRequest),
    /// The approval for this occurrence no longer waits on the client.
    ApprovalSettled(String),
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
    pub(super) parent_session_id: Option<SessionId>,
    pub(super) workspace_root: Option<PathBuf>,
}

pub(super) struct Mapper {
    session_id: SessionId,
    facts: SessionFacts,
    model: Option<String>,
    selection: Option<ModelSelection>,
    message_seq: u64,
    tool_ids: HashMap<String, String>,
    stop_reason: Option<v2::StopReason>,
    state: Option<StateKind>,
    pending_approvals: Vec<ApprovalRequest>,
    /// While set, approvals are only recorded; the bootstrap asks for the
    /// still-pending ones after it has streamed.
    replaying: bool,
}

pub(super) fn acp_session_id(session_id: SessionId) -> v2::SessionId {
    v2::SessionId::new(session_id.as_uuid().to_string())
}

pub(super) fn parse_session_id(id: &v2::SessionId) -> Option<SessionId> {
    uuid::Uuid::parse_str(&id.0).ok().map(SessionId::from_uuid)
}

/// The single `model` config option for a `(provider, model)` pair.
pub(super) fn model_option(provider: &str, model: &str) -> v2::SessionConfigOption {
    let value = acp::encode_model_option_id(provider, model);
    let mut meta = None;
    acp::write_horizon_meta(
        &mut meta,
        &acp::ModelOptionMeta {
            provider: provider.to_string(),
            model: model.to_string(),
        },
    )
    .expect("model option meta serializes");
    let option = v2::SessionConfigSelectOption::new(value.clone(), format!("{provider} · {model}"))
        .meta(meta);
    v2::SessionConfigOption::select(acp::MODEL_CONFIG_ID, "Model", value, vec![option])
        .category(v2::SessionConfigOptionCategory::Model)
}

/// The config options for a session's applied model: the selection's
/// vocabulary when there is one, the provider id and resolved model
/// otherwise, nothing before a model is known.
pub(super) fn model_options(
    provider_id: &str,
    model: Option<&str>,
    selection: Option<&ModelSelection>,
) -> Vec<v2::SessionConfigOption> {
    match (selection, model) {
        (Some(selection), _) => vec![model_option(&selection.provider, &selection.model)],
        (None, Some(model)) => vec![model_option(provider_id, model)],
        (None, None) => Vec::new(),
    }
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
            message_seq: 0,
            tool_ids: HashMap::new(),
            stop_reason: None,
            state: None,
            pending_approvals: Vec::new(),
            replaying: true,
        }
    }

    pub(super) fn session_info_meta(&self) -> acp::SessionInfoMeta {
        acp::SessionInfoMeta {
            workspace_root: self.facts.workspace_root.clone(),
            parent_session_id: self.facts.parent_session_id,
            role_id: self.facts.role_id.clone(),
            provider_id: self.facts.provider_id.clone(),
        }
    }

    pub(super) fn config_options(&self) -> Vec<v2::SessionConfigOption> {
        model_options(
            &self.facts.provider_id,
            self.model.as_deref(),
            self.selection.as_ref(),
        )
    }

    pub(super) fn config_option_update(&self) -> Option<Outgoing> {
        let options = self.config_options();
        (!options.is_empty()).then(|| {
            Outgoing::Update(v2::SessionUpdate::ConfigOptionUpdate(
                v2::ConfigOptionUpdate::new(options),
            ))
        })
    }

    pub(super) fn session_info_update(&self) -> Outgoing {
        Outgoing::Update(v2::SessionUpdate::SessionInfoUpdate(
            v2::SessionInfoUpdate::new().meta(horizon_meta(&self.session_info_meta())),
        ))
    }

    /// Folds bootstrap metadata into the mapper's state without emitting
    /// anything; the bootstrap emits the combined result itself.
    pub(super) fn absorb_metadata(&mut self, event: &AgentWireEvent) {
        match event {
            AgentWireEvent::SessionModel(model) => self.model = Some(model.clone()),
            AgentWireEvent::SessionSelection(selection) => self.selection = Some(selection.clone()),
            AgentWireEvent::WorkspaceRootResolved(resolved) => {
                self.facts.workspace_root = Some(resolved.workspace_root.clone());
                self.facts.parent_session_id = resolved.parent_session_id;
            }
            _ => {}
        }
    }

    /// Ends the bootstrap: from here on approvals are asked as they arrive.
    /// Returns the approvals the replayed history left pending.
    pub(super) fn finish_replay(&mut self) -> Vec<ApprovalRequest> {
        self.replaying = false;
        self.pending_approvals.clone()
    }

    pub(super) fn map(&mut self, event: &AgentWireEvent) -> Vec<Outgoing> {
        let session_id = self.session_id;
        match event {
            AgentWireEvent::Event(event) => self.map_event(event),
            AgentWireEvent::ToolCallProgress(progress) => {
                vec![Outgoing::ToolCallProgress(
                    acp::ToolCallProgressNotification {
                        session_id,
                        event: acp::ToolCallProgressEvent::Progress {
                            key: progress.key.clone(),
                            tool_id: progress.tool_id.clone(),
                            bytes: progress.bytes,
                        },
                    },
                )]
            }
            AgentWireEvent::ToolCallProgressClosed(key) => {
                vec![Outgoing::ToolCallProgress(
                    acp::ToolCallProgressNotification {
                        session_id,
                        event: acp::ToolCallProgressEvent::Closed { key: key.clone() },
                    },
                )]
            }
            AgentWireEvent::TaskProgress(progress) => {
                vec![Outgoing::TaskProgress(task_progress(session_id, progress))]
            }
            AgentWireEvent::SessionModel(_) | AgentWireEvent::SessionSelection(_) => {
                self.absorb_metadata(event);
                self.config_option_update().into_iter().collect()
            }
            AgentWireEvent::WorkspaceRootResolved(_) => {
                self.absorb_metadata(event);
                vec![self.session_info_update()]
            }
            AgentWireEvent::ReplayStarted
            | AgentWireEvent::ReplayComplete
            | AgentWireEvent::AttachmentClosed(_) => Vec::new(),
        }
    }

    fn next_message_id(&self) -> v2::MessageId {
        v2::MessageId::new(format!("msg-{}", self.message_seq))
    }

    fn map_event(&mut self, event: &Event) -> Vec<Outgoing> {
        let session_id = self.session_id;
        let session_event = |event| vec![Outgoing::SessionEvent(event)];
        match event {
            Event::MessageCommitted(message) => {
                let id = self.next_message_id();
                self.message_seq += 1;
                let content = vec![v2::ContentBlock::from(message.text.clone())];
                let update = match message.role {
                    MessageRole::Assistant => {
                        v2::SessionUpdate::AgentMessage(v2::AgentMessage::new(id).content(content))
                    }
                    MessageRole::User => {
                        v2::SessionUpdate::UserMessage(v2::UserMessage::new(id).content(content))
                    }
                    MessageRole::TaskNotification | MessageRole::AutoContinue => {
                        let role = if message.role == MessageRole::TaskNotification {
                            acp::MessageRole::TaskNotification
                        } else {
                            acp::MessageRole::AutoContinue
                        };
                        v2::SessionUpdate::UserMessage(
                            v2::UserMessage::new(id)
                                .content(content)
                                .meta(horizon_meta(&acp::MessageMeta { role })),
                        )
                    }
                };
                vec![Outgoing::Update(update)]
            }
            Event::AssistantTextDelta(delta) => {
                vec![Outgoing::Update(v2::SessionUpdate::AgentMessageChunk(
                    v2::ContentChunk::new(delta.text.clone().into(), self.next_message_id()),
                ))]
            }
            Event::ReasoningDelta(delta) => {
                vec![Outgoing::Update(v2::SessionUpdate::AgentThoughtChunk(
                    v2::ContentChunk::new(
                        delta.text.clone().into(),
                        v2::MessageId::new(format!("thought-{}", self.message_seq)),
                    ),
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
                    .raw_input(request.input.0.clone())
                    .meta(horizon_meta(&acp::ToolCallMeta {
                        call_id: request.call_id.0.clone(),
                        tool_id: request.tool_id.clone(),
                        outcome: None,
                        auto_approved: None,
                        policy_tier: None,
                    }));
                vec![Outgoing::Update(v2::SessionUpdate::ToolCallUpdate(update))]
            }
            Event::ToolCallStarted(identity) => {
                vec![Outgoing::Update(v2::SessionUpdate::ToolCallUpdate(
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
                let output = &result.output.0;
                let meta = acp::ToolCallMeta {
                    call_id: result.call_id.0.clone(),
                    tool_id: self.tool_ids.get(&occurrence).cloned().unwrap_or_default(),
                    outcome: Some(tool_outcome(&result.outcome)),
                    auto_approved: output.get("auto_approved").and_then(|v| v.as_bool()),
                    policy_tier: output
                        .get("policy_tier")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                };
                let update = v2::ToolCallUpdate::new(occurrence.clone())
                    .status(status)
                    .raw_output(output.clone())
                    .meta(horizon_meta(&meta));
                let mut out = vec![Outgoing::Update(v2::SessionUpdate::ToolCallUpdate(update))];
                out.extend(self.settle_approval(&occurrence));
                out
            }
            Event::ApprovalRequested(request) => {
                self.pending_approvals.push(request.clone());
                if self.replaying {
                    Vec::new()
                } else {
                    vec![Outgoing::AskPermission(request.clone())]
                }
            }
            Event::ApprovalResolved(resolved) => self.settle_approval(&resolved.occurrence_id.0),
            Event::TurnEnded(reason) => {
                self.stop_reason = Some(stop_reason(*reason));
                Vec::new()
            }
            Event::StateChanged(state) => self.map_state(*state),
            Event::Error(error) => session_event(acp::SessionEventNotification::Error {
                session_id,
                message: error.message.clone(),
            }),
            Event::Exited(exit) => session_event(acp::SessionEventNotification::Exited {
                session_id,
                reason: exit.reason.clone(),
            }),
            Event::SessionResumed => {
                session_event(acp::SessionEventNotification::SessionResumed { session_id })
            }
            Event::ProviderRateLimited(limited) => {
                session_event(acp::SessionEventNotification::ProviderRateLimited {
                    session_id,
                    status: limited.status,
                    attempt: limited.attempt,
                    backoff_ms: limited.backoff_ms,
                })
            }
            Event::HistoryCleared(cleared) => {
                session_event(acp::SessionEventNotification::HistoryCleared {
                    session_id,
                    cleared_occurrence_ids: cleared
                        .cleared_occurrence_ids
                        .iter()
                        .map(|id| id.0.clone())
                        .collect(),
                    recovered_chars: cleared.recovered_chars,
                })
            }
            Event::MemoryDigest(digest) => vec![Outgoing::Memory(acp::MemoryNotification {
                session_id,
                event: acp::MemoryEvent::Digest(memory_digest(digest)),
            })],
            Event::MemoryCheckpointMissed => vec![Outgoing::Memory(acp::MemoryNotification {
                session_id,
                event: acp::MemoryEvent::CheckpointMissed,
            })],
            Event::ProviderRequestSent(sent) => provider_request(
                session_id,
                acp::ProviderRequestEvent::Sent {
                    model: sent.model.clone(),
                },
            ),
            Event::ProviderRequestFirstToken => {
                provider_request(session_id, acp::ProviderRequestEvent::FirstToken)
            }
            Event::ProviderRequestFinished => {
                provider_request(session_id, acp::ProviderRequestEvent::Finished)
            }
            Event::InputAccepted(_)
            | Event::InputStarted(_)
            | Event::InputQueuePaused(_)
            | Event::InputOutcome(_)
            | Event::DeliveryAcknowledged(_)
            | Event::SessionInputSent { .. }
            | Event::EnvironmentReady { .. }
            | Event::EnvironmentActivated(_)
            | Event::EnvironmentActivationFailed(_)
            | Event::MoaPassStarted(_)
            | Event::MemorySeeded
            | Event::ContinueTurnRequested(_)
            | Event::ConversationRecorded(_)
            | Event::ProviderRequestUsage(_) => Vec::new(),
        }
    }

    fn settle_approval(&mut self, occurrence: &str) -> Vec<Outgoing> {
        let before = self.pending_approvals.len();
        self.pending_approvals
            .retain(|request| request.occurrence_id.0 != occurrence);
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
            | SessionState::Completed
            | SessionState::Cancelled
            | SessionState::Failed
            | SessionState::Terminated => (
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
        vec![Outgoing::Update(v2::SessionUpdate::StateUpdate(update))]
    }
}

fn provider_request(session_id: SessionId, event: acp::ProviderRequestEvent) -> Vec<Outgoing> {
    vec![Outgoing::ProviderRequest(
        acp::ProviderRequestNotification { session_id, event },
    )]
}

pub(super) fn tool_kind(tool_id: &str) -> v2::ToolKind {
    match tool_id {
        "fs.read" => v2::ToolKind::Read,
        "fs.edit" | "fs.write" => v2::ToolKind::Edit,
        "bash" => v2::ToolKind::Execute,
        "web_fetch" => v2::ToolKind::Fetch,
        "web_search" | "fs.grep" | "fs.glob" => v2::ToolKind::Search,
        _ => v2::ToolKind::Other,
    }
}

fn stop_reason(reason: TurnEndReason) -> v2::StopReason {
    match reason {
        TurnEndReason::Completed => v2::StopReason::EndTurn,
        TurnEndReason::Cancelled => v2::StopReason::Cancelled,
        TurnEndReason::HaltedByIterationCap => v2::StopReason::MaxTurnRequests,
        TurnEndReason::Failed => v2::StopReason::Other(acp::STOP_REASON_FAILED.to_string()),
        TurnEndReason::HaltedByDoomLoop => {
            v2::StopReason::Other(acp::STOP_REASON_DOOM_LOOP.to_string())
        }
    }
}

pub(super) fn tool_outcome(outcome: &contract::ToolOutcome) -> acp::ToolOutcome {
    match outcome {
        contract::ToolOutcome::Succeeded => acp::ToolOutcome::Succeeded,
        contract::ToolOutcome::Failed => acp::ToolOutcome::Failed,
        contract::ToolOutcome::Denied => acp::ToolOutcome::Denied,
        contract::ToolOutcome::Cancelled => acp::ToolOutcome::Cancelled,
        contract::ToolOutcome::Superseded {
            retry_occurrence_id,
        } => acp::ToolOutcome::Superseded {
            retry_occurrence_id: retry_occurrence_id.0.clone(),
        },
    }
}

fn tool_result(result: &contract::ToolCallResult) -> acp::ToolResult {
    acp::ToolResult {
        call_id: result.call_id.0.clone(),
        occurrence_id: result.occurrence_id.0.clone(),
        output: result.output.0.clone(),
        outcome: tool_outcome(&result.outcome),
    }
}

pub(super) fn approval_kind(kind: &contract::ApprovalKind) -> acp::ApprovalKind {
    match kind {
        contract::ApprovalKind::Standard => acp::ApprovalKind::Standard,
        contract::ApprovalKind::DomainDenialRetry {
            domains,
            prior_result,
        } => acp::ApprovalKind::DomainDenialRetry {
            domains: domains.clone(),
            prior_result: tool_result(prior_result),
        },
        contract::ApprovalKind::FilesystemDenialRetry {
            denials,
            grants,
            prior_result,
        } => acp::ApprovalKind::FilesystemDenialRetry {
            denials: denials.clone(),
            grants: grants.clone(),
            prior_result: tool_result(prior_result),
        },
        contract::ApprovalKind::DomainGrant { domains } => acp::ApprovalKind::DomainGrant {
            domains: domains.clone(),
        },
        contract::ApprovalKind::GitOperation { writable_roots } => {
            acp::ApprovalKind::GitOperation {
                writable_roots: writable_roots.clone(),
            }
        }
        contract::ApprovalKind::MachServiceGrant {
            services,
            prior_result,
        } => acp::ApprovalKind::MachServiceGrant {
            services: services.clone(),
            prior_result: tool_result(prior_result),
        },
    }
}

/// A short title for an approval prompt, by what the approval grants.
pub(super) fn approval_title(kind: &contract::ApprovalKind) -> &'static str {
    match kind {
        contract::ApprovalKind::Standard => "Run tool",
        contract::ApprovalKind::DomainDenialRetry { .. } => "Allow network access and retry",
        contract::ApprovalKind::FilesystemDenialRetry { .. } => "Allow filesystem access and retry",
        contract::ApprovalKind::DomainGrant { .. } => "Allow network access",
        contract::ApprovalKind::GitOperation { .. } => "Allow Git metadata writes",
        contract::ApprovalKind::MachServiceGrant { .. } => "Allow system services and retry",
    }
}

fn task_progress(
    session_id: SessionId,
    progress: &contract::TaskProgress,
) -> acp::TaskProgressNotification {
    acp::TaskProgressNotification {
        session_id,
        task_session_id: progress.task_session_id,
        description: progress.description.clone(),
        state: match progress.state {
            contract::TaskProgressState::Running => acp::TaskProgressState::Running,
            contract::TaskProgressState::Finished => acp::TaskProgressState::Finished,
        },
        activity: progress.activity.clone(),
        started_at_epoch_ms: progress.started_at_epoch_ms,
    }
}

fn memory_digest(digest: &contract::MemoryDigest) -> acp::MemoryDigest {
    acp::MemoryDigest {
        updates: digest
            .updates
            .iter()
            .map(|update| acp::MemoryFieldUpdate {
                field: match update.field {
                    contract::MemoryField::Goal => acp::MemoryField::Goal,
                    contract::MemoryField::Decisions => acp::MemoryField::Decisions,
                    contract::MemoryField::Completed => acp::MemoryField::Completed,
                    contract::MemoryField::InProgress => acp::MemoryField::InProgress,
                    contract::MemoryField::Stuck => acp::MemoryField::Stuck,
                    contract::MemoryField::NextStep => acp::MemoryField::NextStep,
                    contract::MemoryField::Related => acp::MemoryField::Related,
                },
                op: match update.op {
                    contract::MemoryOp::Set => acp::MemoryOp::Set,
                    contract::MemoryOp::Append => acp::MemoryOp::Append,
                    contract::MemoryOp::Clear => acp::MemoryOp::Clear,
                },
                content: update.content.clone(),
            })
            .collect(),
        folded_log_range: digest.folded_log_range.map(|range| acp::FoldedLogRange {
            from_seq: range.from_seq,
            to_seq: range.to_seq,
        }),
        no_update_reason: digest.no_update_reason.clone(),
    }
}

#[cfg(test)]
mod tests;
