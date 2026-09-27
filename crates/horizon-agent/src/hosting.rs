//! The vocabulary between `horizon-agentd`'s session threads and the
//! connections serving them: the per-attachment event stream
//! ([`AgentWireEvent`]), the spawn request and listing summaries, and the
//! host-tool exchange. `horizon-agentd` maps these onto ACP v2 and the
//! `_horizon/*` extensions (`crates/horizon-acp`,
//! `docs/acp-agentd-design.md`); none of these types is serialized onto a
//! socket itself.
//!
//! This module references [`crate::contract`] types (`Event`, `SessionId`,
//! ...); nothing in `contract` references this module.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::contract::{
    Event, JsonValue, ProviderEvent, ProviderId, RequestId, SessionId, TaskProgress,
    ToolCallProgress,
};
use crate::roles::RoleId;

/// Everything a hosted agent session pushes to its attachment: the
/// session's provider events plus the session-scoped announcements.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AgentWireEvent {
    /// Start of this attachment's private history and metadata snapshot.
    ReplayStarted,
    /// Every snapshot item precedes this marker; live updates follow it.
    ReplayComplete,
    /// The attachment ended; the session itself may still be running.
    AttachmentClosed(AttachmentEnd),
    /// A folded provider event — the transcript's raw material, identical
    /// to what the event log persists.
    Event(Event),
    /// Ephemeral tool-call-argument-streaming preview
    /// (`contract::ProviderEvent::tool_call_progress`). UI-only feedback:
    /// never part of conversation history and never persisted (see
    /// [`ToolCallProgress`]'s own doc comment), which is why
    /// `contract::Event` deliberately has no variant for it.
    ToolCallProgress(ToolCallProgress),
    /// The session's resolved model id — sent once right after a fresh
    /// session resolves it, and re-announced to every later attachment
    /// (see `docs/agent-output-ui-amendment.md`'s dated model-chip
    /// addendum). Ephemeral like `ToolCallProgress`.
    SessionModel(String),
    /// Live correction of a freshly isolated session's authoritative
    /// `workspace_root` (and derivation edge) — sent once, right after
    /// `horizon-agentd` resolves the session's isolated worktree, which
    /// only finishes *after* `session/new` already returned. Not sent at all
    /// when isolation fails and degrades to a shared spawn (nothing to
    /// correct then, mirroring [`SessionSummary::parent_session_id`]'s
    /// "the edge exists only via isolation").
    WorkspaceRootResolved(WorkspaceRootResolved),
    /// Live progress of one of this session's background `task` children —
    /// the daemon-side task watcher mirroring what the child was last
    /// observed doing. Ephemeral like [`ToolCallProgress`]: a completion is
    /// recorded durably as a `MessageRole::TaskNotification` message; this
    /// event only drives the client's live progress rows. The daemon retains
    /// current running rows and seeds them during attachment bootstrap.
    TaskProgress(TaskProgress),
    /// The session's last applied *selection* — the `(provider, model)` pair
    /// a `set_session_model` call named (an echo of
    /// `contract::Command::SetSessionModel`), where `model` is the value the
    /// caller asked for: a `[[moa]]` entry name for the reserved `moa` group,
    /// a model id otherwise. Display-only and ephemeral like
    /// `ToolCallProgress`: the composer's model chip renders `provider ·
    /// model` (`moa · mix`) instead of the aggregator's resolved model id
    /// that [`Self::SessionModel`] carries.
    SessionSelection(ModelSelection),
    /// Close the preview identified by its streaming key.
    ToolCallProgressClosed(String),
}

/// Why an attachment ended. Reattach to obtain a fresh, complete snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AttachmentEnd {
    Replaced,
    Lagged,
    Detached,
    SessionEnded,
}

impl AgentWireEvent {
    pub fn from_provider(event: &ProviderEvent) -> Option<Self> {
        Some(match event {
            ProviderEvent::Event { event, .. } => Self::Event(event.clone()),
            ProviderEvent::ToolCallProgress(progress) => Self::ToolCallProgress(progress.clone()),
            ProviderEvent::SessionModel(model) => Self::SessionModel(model.clone()),
            ProviderEvent::SessionSelection(selection) => Self::SessionSelection(selection.clone()),
            ProviderEvent::TaskProgress(progress) => Self::TaskProgress(progress.clone()),
            ProviderEvent::ToolCallProgressClosed(key) => Self::ToolCallProgressClosed(key.clone()),
            ProviderEvent::SettleTools { .. } => return None,
        })
    }
}

/// [`AgentWireEvent::SessionSelection`]'s payload — the `(provider, model)`
/// pair a switch named, echoing `contract::Command::SetSessionModel`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelSelection {
    /// The `[[providers]]` / `[[moa]]` entry name the picker or CLI named
    /// (the legacy `[provider]` fold-in resolves as `default`).
    pub provider: String,
    /// The model the caller asked for: a `[[moa]]` entry name for the
    /// reserved `moa` provider, a model id otherwise.
    pub model: String,
}

/// [`AgentWireEvent::WorkspaceRootResolved`]'s payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceRootResolved {
    pub workspace_root: PathBuf,
    /// Additive, like [`SessionSummary::parent_session_id`] -- `None` for an
    /// isolated-but-sourceless spawn (still a valid lineage root, see that
    /// field's own doc comment).
    #[serde(default)]
    pub parent_session_id: Option<SessionId>,
}

/// One configured provider as `_horizon/list_providers` reports it —
/// the model picker's per-provider data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderSummary {
    /// The `[[providers]]` `name` — what `set_session_model`'s `provider`
    /// argument names. The legacy `[provider]` fold-in resolves as
    /// `default`.
    pub name: String,
    /// The entry's file `base_url`, if any. The env-resolved value lives in
    /// the registry's rig view; the picker needs only what the file named.
    pub base_url: Option<String>,
    /// The environment variable **name** the entry's API key is read from —
    /// recorded so a picker can explain unavailability ("ANTHROPIC_API_KEY
    /// is not set"). Never a value (the secrets-stay-out rule).
    pub api_key_env: String,
    /// The model this entry runs when nothing has selected one, if the file
    /// names it. `None` leaves the kind's own built-in default in place. The
    /// picker's candidate ids come from the provider's own `/models`
    /// listing (`_horizon/list_provider_models`), not from here.
    pub default_model: Option<String>,
    /// Build-time resolved (the entry's key variable was set when the
    /// provider surface was built). `false` = registered but unavailable
    /// (grayed out, not hidden). A mid-session environment change is
    /// honored by a switch, not by this listing.
    pub available: bool,
    /// Whether this is the surface's default entry (`default_provider`, or
    /// the first entry when unset / the legacy fold-in).
    pub default: bool,
}

/// One entry of a `session/list` reply.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
    /// So a (re)connecting client can label a resumed/live session by its
    /// role without a separate round trip -- mirrors `provider_id` above.
    pub role_id: Option<RoleId>,
    /// The session this one derives from, per
    /// `docs/session-relationship-design.md` decisions 1-3: set only when
    /// this session was actually spawned isolated (its own git worktree
    /// branched from the source session's directory) -- a shared-directory
    /// spawn creates no edge, so this stays `None` even if a spawn source
    /// was given. New event-log records persist this edge with the
    /// authoritative root/isolation context, so a resumed isolated session
    /// reports it again after its linked worktree has been revalidated.
    pub parent_session_id: Option<SessionId>,
    /// This session's *actual* confinement directory, as `horizon-agentd`
    /// itself resolved it -- the authoritative counterpart to `SessionNew.
    /// workspace_root` (that field is only ever the caller's pre-spawn
    /// value; for an isolated session, `horizon-agentd` overrides it with
    /// the worktree path it creates, which the caller cannot know in
    /// advance since worktree creation finishes asynchronously, after
    /// `session/new` already returned -- see
    /// `session::resolve_and_create_isolated_worktree`). Populated from the
    /// same `SessionEntry.workspace_root` a resumed session's summary reads
    /// too. New event-log records persist this authoritative value, and
    /// isolated roots are revalidated against Git before resume. Read by
    /// `WorkspaceShell::spawn_agent_resume`/`spawn_workspace_restore` to
    /// correct the workspace model's stored root for a session it adopts.
    pub workspace_root: Option<PathBuf>,
}

/// Per `docs/agent-runtime-split-design.md` guardrail 5, spawning a fresh
/// session (`session/new`) is distinct from attaching to an existing one
/// (`session/resume`) and carries per-session overrides.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionNew {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
    pub role_id: Option<RoleId>,
    /// The directory `horizon-agentd`'s file tools should confine this
    /// session to (`tools::state::ToolSessionState::workspace_root`).
    /// `None` keeps today's behavior -- the session falls back to
    /// `horizon-agentd`'s own process cwd (`session::run_session`'s
    /// `ToolSessionState::for_current_dir` call). Passed into
    /// `AgentdHandle::start_session` by the workspace layer
    /// (`WorkspaceShell::reconcile`), which computes the Horizon process's
    /// own cwd once per session (falling back to `None` only if that cwd
    /// can't be read) and records the same value on the session's
    /// `WorkspaceSession::workspace_root` -- so a session's workspace root
    /// tracks whichever Horizon window spawned it, not `horizon-agentd`'s
    /// own cwd (one shared, long-lived daemon per user, started from
    /// whatever directory happened to be current the first time it was
    /// launched).
    pub workspace_root: Option<PathBuf>,
    /// The pane/session this spawn was invoked "from" -- e.g. the split
    /// target, or whatever pane was active/named at spawn time. Independent
    /// of `isolate` (decision 3's two knobs): carried even for a
    /// shared-directory spawn, but only turns into a recorded
    /// `SessionSummary.parent_session_id` lineage edge when `isolate` is
    /// also true (decision 2: "the edge exists only via isolation"). `None`
    /// for a spawn with no source pane at all (e.g. a fresh tab with
    /// nothing active).
    pub spawn_source_session_id: Option<SessionId>,
    /// Whether `horizon-agentd` should give this session its own git
    /// worktree, branched from `spawn_source_session_id`'s directory,
    /// instead of confining it to `workspace_root` directly -- decision 3's
    /// per-spawn isolation knob. The origin-based default (palette: shared;
    /// CLI/control-plane: isolated) plus any explicit per-spawn override are
    /// both resolved client-side before this ever reaches the daemon;
    /// `horizon-agentd` just executes whatever concrete choice arrives
    /// here (see `docs/session-relationship-design.md` decision 3).
    pub isolate: bool,
}

/// The agent (child) asking the client to run a host-coupled tool (e.g.
/// `workspace.snapshot`) over this same connection -- guardrail 4. Sent as
/// `_horizon/host_tool`; the `request_id` correlates the reply because a
/// session thread blocks on the matching [`HostToolResponse`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostToolRequest {
    pub request_id: RequestId,
    pub tool_id: String,
    pub input: JsonValue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostToolResponse {
    pub request_id: RequestId,
    pub output: JsonValue,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_wire_event_round_trips_each_variant() {
        let events = vec![
            AgentWireEvent::ReplayStarted,
            AgentWireEvent::ReplayComplete,
            AgentWireEvent::AttachmentClosed(AttachmentEnd::Replaced),
            AgentWireEvent::AttachmentClosed(AttachmentEnd::Lagged),
            AgentWireEvent::AttachmentClosed(AttachmentEnd::Detached),
            AgentWireEvent::AttachmentClosed(AttachmentEnd::SessionEnded),
            AgentWireEvent::Event(Event::ToolCallRequested(crate::contract::ToolCallRequest {
                call_id: (crate::contract::ToolCallId("call-1".to_string())).clone(),
                tool_id: "fs.read".to_string(),
                input: serde_json::json!({"path": "a.txt"}).into(),
                occurrence_id: crate::contract::OccurrenceId(
                    (crate::contract::ToolCallId("call-1".to_string()))
                        .0
                        .clone(),
                ),
            })),
            AgentWireEvent::ToolCallProgress(ToolCallProgress {
                key: "call-1".to_string(),
                tool_id: Some("fs.read".to_string()),
                bytes: 64,
            }),
            AgentWireEvent::ToolCallProgressClosed("call-1".into()),
            AgentWireEvent::SessionModel("gpt-4o".to_string()),
            AgentWireEvent::WorkspaceRootResolved(WorkspaceRootResolved {
                workspace_root: PathBuf::from("/tmp/some-workspace/.horizon/worktrees/abcd1234"),
                parent_session_id: Some(SessionId::new()),
            }),
            AgentWireEvent::SessionSelection(ModelSelection {
                provider: "moa".to_string(),
                model: "mix".to_string(),
            }),
        ];
        for event in events {
            let encoded = serde_json::to_value(&event).unwrap();
            let decoded: AgentWireEvent = serde_json::from_value(encoded).unwrap();
            assert_eq!(decoded, event);
        }
    }
}
