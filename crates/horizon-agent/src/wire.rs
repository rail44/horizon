//! The agent domain's wire vocabulary for `horizon-sessiond`'s socket —
//! the payload types that cross the process boundary, and nothing else.
//!
//! The v10 remoc cutover (`docs/remoc-adoption-design.md` §2) deleted this
//! module's JSONL machinery wholesale: the `Envelope`/`EnvelopeBody` pair,
//! the `agent_control`/`agent_command`/`agent_event` kind constants, the
//! `Control` dispatch enum, and the `encode_*`/`decode_*`/`read_*`/
//! `write_*` framing helpers. What used to be `Control` variants maps onto
//! `horizon_session_protocol::SessionHub` instead: `SessionList`/`SessionNew`/
//! `SessionLoad` are rtc calls (`list_agents`/`new_agent`/`attach_agent`),
//! `HostToolRequest`/`HostToolResponse` ride connection-global channels
//! handed over in `HubHello`, and the session-scoped announcements ride the
//! per-attachment [`AgentWireEvent`] channel.
//!
//! **Guardrail 1 (contract ≠ wire)** still holds: this module references
//! [`crate::contract`] types (`Command`, `Event`, `SessionId`, ...); nothing
//! in `contract` references this module. And the vocabulary stays
//! serde-plain and remoc-free — the hub trait that names these types lives
//! in `horizon-session-protocol`, keeping the exit cost of a transport
//! re-swap bounded (`docs/remoc-adoption-design.md` §1).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::contract::{Event, JsonValue, ProviderId, RequestId, SessionId, ToolCallProgress};
use crate::roles::RoleId;

/// Everything a hosted agent session pushes to its attached client, on the
/// attachment's event channel (`horizon_session_protocol::AgentAttachment::
/// events`): the session's provider events, plus the session-scoped
/// announcements that were their own control envelopes on the JSONL wire.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum AgentWireEvent {
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
    /// `horizon-sessiond` resolves the session's isolated worktree, which
    /// only finishes *after* `new_agent` already returned. Not sent at all
    /// when isolation fails and degrades to a shared spawn (nothing to
    /// correct then, mirroring [`SessionSummary::parent_session_id`]'s
    /// "the edge exists only via isolation").
    WorkspaceRootResolved(WorkspaceRootResolved),
    /// Skew catch-all — `#[serde(other)]`: an event this build can't name
    /// decodes to `Unknown` on the Postbag wire, payload discarded (the
    /// receiver skips it; under serde_json only unit variants degrade). Keep last.
    #[serde(other)]
    Unknown,
}

/// [`AgentWireEvent::WorkspaceRootResolved`]'s payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceRootResolved {
    pub workspace_root: PathBuf,
    /// Additive, like [`SessionSummary::parent_session_id`] -- `None` for an
    /// isolated-but-sourceless spawn (still a valid lineage root, see that
    /// field's own doc comment).
    #[serde(default)]
    pub parent_session_id: Option<SessionId>,
}

/// One entry of a `SessionHub::list_agents` reply.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
    /// So a (re)connecting client can label a resumed/live session by its
    /// role without a separate round trip -- mirrors `provider_id` above.
    #[serde(default)]
    pub role_id: Option<RoleId>,
    /// The session this one derives from, per
    /// `docs/session-relationship-design.md` decisions 1-3: set only when
    /// this session was actually spawned isolated (its own git worktree
    /// branched from the source session's directory) -- a shared-directory
    /// spawn creates no edge, so this stays `None` even if a spawn source
    /// was given. `#[serde(default)]`, purely additive like `SessionNew.
    /// workspace_root` below -- no version bump. A resumed session
    /// (`session::resume_persisted_sessions`) always reports `None` here
    /// today: lineage lives in `horizon-sessiond`'s in-memory
    /// `SessiondState`, not the event log, so it doesn't survive a
    /// `horizon-sessiond` process restart -- the same accepted gap
    /// `SessionNew.workspace_root` already has (see that field's doc
    /// comment).
    #[serde(default)]
    pub parent_session_id: Option<SessionId>,
    /// This session's *actual* confinement directory, as `horizon-sessiond`
    /// itself resolved it -- the authoritative counterpart to `SessionNew.
    /// workspace_root` (that field is only ever the caller's pre-spawn
    /// value; for an isolated session, `horizon-sessiond` overrides it with
    /// the worktree path it creates, which the caller cannot know in
    /// advance since worktree creation finishes asynchronously, after
    /// `new_agent` already returned -- see
    /// `session::resolve_and_create_isolated_worktree`). Additive, like
    /// `parent_session_id` above; populated from the same `SessionEntry.
    /// workspace_root` a resumed session's summary reads too, so this
    /// *does* survive a `horizon-sessiond` restart even though `parent_
    /// session_id` doesn't -- a resumed session's `SessionEntry.
    /// workspace_root` is `None` today regardless (see `SessionNew.
    /// workspace_root`'s "resumed sessions don't persist it" note), so in
    /// practice this is `None` for a resumed session too, but for a
    /// different, narrower reason (no persisted value to resume from, not
    /// "lineage doesn't survive a restart"). Read by `WorkspaceShell::
    /// spawn_agent_resume`/`spawn_workspace_restore` to correct the
    /// workspace model's stored root for a session it adopts.
    #[serde(default)]
    pub workspace_root: Option<PathBuf>,
}

/// Per `docs/agent-runtime-split-design.md` guardrail 5, spawning a fresh
/// session (`SessionHub::new_agent`) is distinct from attaching to an
/// existing one (`attach_agent`) and carries per-session overrides.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionNew {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
    #[serde(default)]
    pub role_id: Option<RoleId>,
    /// The directory `horizon-sessiond`'s file tools should confine this
    /// session to (`tools::state::ToolSessionState::workspace_root`).
    /// `None` keeps today's behavior -- the session falls back to
    /// `horizon-sessiond`'s own process cwd (`session::run_session`'s
    /// `ToolSessionState::for_current_dir` call). Unlike `role_id` above,
    /// this is a brand-new field, not a reshape of an existing one, so it's
    /// purely additive: `#[serde(default)]` lets a peer's `SessionNew`
    /// written before this field existed still parse (as `None`), mirroring
    /// `persistence::event_log::Record::role_id`'s own additive-field
    /// precedent. Passed into `SessiondHandle::start_session` by the
    /// workspace layer (`WorkspaceShell::reconcile`), which computes the
    /// Horizon process's own cwd once per session (falling back to `None`
    /// only if that cwd can't be read) and records the same value on the
    /// session's `WorkspaceSession::workspace_root` -- so a session's
    /// workspace root tracks whichever Horizon window spawned it, not
    /// `horizon-sessiond`'s own cwd (one shared, long-lived daemon per
    /// user, started from whatever directory happened to be current the
    /// first time it was launched).
    #[serde(default)]
    pub workspace_root: Option<PathBuf>,
    /// The pane/session this spawn was invoked "from" -- e.g. the split
    /// target, or whatever pane was active/named at spawn time. Independent
    /// of `isolate` (decision 3's two knobs): carried even for a
    /// shared-directory spawn, but only turns into a recorded
    /// `SessionSummary.parent_session_id` lineage edge when `isolate` is
    /// also true (decision 2: "the edge exists only via isolation"). `None`
    /// for a spawn with no source pane at all (e.g. a fresh tab with
    /// nothing active). Additive, like `workspace_root` above.
    #[serde(default)]
    pub spawn_source_session_id: Option<SessionId>,
    /// Whether `horizon-sessiond` should give this session its own git
    /// worktree, branched from `spawn_source_session_id`'s directory,
    /// instead of confining it to `workspace_root` directly -- decision 3's
    /// per-spawn isolation knob. The origin-based default (palette: shared;
    /// CLI/control-plane: isolated) plus any explicit per-spawn override are
    /// both resolved client-side before this ever reaches the wire;
    /// `horizon-sessiond` just executes whatever concrete choice arrives
    /// here (see `docs/session-relationship-design.md` decision 3). `false`
    /// (via `#[serde(default)]`) reproduces today's shared-directory
    /// behavior for a peer built before this field existed.
    #[serde(default)]
    pub isolate: bool,
}

/// The agent (child) asking the client to run a host-coupled tool (e.g.
/// `workspace.snapshot`) over this same connection -- guardrail 4. Rides
/// the connection-global `HubHello::host_tools` channel; the `request_id`
/// correlation survives the cutover because the exchange is genuinely
/// asynchronous on the daemon side (a session thread blocks on the
/// matching [`HostToolResponse`]).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HostToolRequest {
    pub request_id: RequestId,
    pub tool_id: String,
    pub input: JsonValue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HostToolResponse {
    pub request_id: RequestId,
    pub output: JsonValue,
}

#[cfg(test)]
mod tests {
    use super::*;

    // The four `contract_version_*` pin tests that lived here -- each a
    // hand-maintained `assert_eq!(CONTRACT_VERSION, 9)` whose doc comment
    // re-argued why a given change was or wasn't a bump -- are retired
    // (`docs/remoc-adoption-design.md` §4 rule 4). Their job, forcing a
    // human decision on every wire-shape change, now belongs to the
    // committed schema artifact and its checkers: any wire change shows up
    // as a diff of `crates/horizon-session-protocol/schema/session-wire.json`
    // (drift-enforced by `crates/horizon-sessiond/tests/wire_schema.rs`,
    // where the v1-v10 bump history those tests narrated now lives), and
    // `scripts/check-wire-schema.sh` fails any non-additive change that
    // doesn't bump `SESSION_PROTOCOL_VERSION` alongside it.

    /// An event variant from a future build decodes as
    /// [`AgentWireEvent::Unknown`] -- the §4 skew catch-all. serde_json can
    /// only prove the unit-variant case (`#[serde(other)]` insists on unit
    /// content there); the payload-carrying case is proven under the wire
    /// codec in `horizon-session-protocol/tests/skew.rs`.
    #[test]
    fn unknown_unit_event_variant_decodes_to_unknown_not_an_error() {
        let event: AgentWireEvent = serde_json::from_value(serde_json::json!("SessionTeleport"))
            .expect("an unknown unit variant should still parse");
        assert_eq!(event, AgentWireEvent::Unknown);
    }

    /// A `SessionNew` written by a peer built before `workspace_root`
    /// existed has no such key at all -- `#[serde(default)]` must still
    /// parse it (as `None`), not reject the payload. Mirrors
    /// `persistence::event_log::Record`'s
    /// `reads_a_pre_role_record_with_no_role_id_key` regression guard.
    #[test]
    fn session_new_without_optional_keys_takes_the_defaults() {
        let session_id = SessionId::new();
        let new: SessionNew = serde_json::from_value(serde_json::json!({
            "session_id": session_id,
            "provider_id": "builtin.agent.rig",
            "role_id": null,
        }))
        .expect("payload should parse despite the missing optional keys");
        assert_eq!(new.workspace_root, None);
        assert_eq!(new.spawn_source_session_id, None);
        assert!(!new.isolate);
    }

    /// Same for `SessionSummary`'s additive fields on the reply side.
    #[test]
    fn session_summary_without_optional_keys_takes_the_defaults() {
        let session_id = SessionId::new();
        let summary: SessionSummary = serde_json::from_value(serde_json::json!({
            "session_id": session_id,
            "provider_id": "builtin.agent.rig",
            "role_id": null,
        }))
        .expect("summary should parse despite the missing optional keys");
        assert_eq!(summary.parent_session_id, None);
        assert_eq!(summary.workspace_root, None);
    }

    #[test]
    fn agent_wire_event_round_trips_each_variant() {
        let events = vec![
            AgentWireEvent::Event(Event::ToolCallRequested(crate::contract::ToolCallRequest {
                call_id: crate::contract::ToolCallId("call-1".to_string()),
                tool_id: "fs.read".to_string(),
                input: serde_json::json!({"path": "a.txt"}).into(),
            })),
            AgentWireEvent::ToolCallProgress(ToolCallProgress {
                key: "call-1".to_string(),
                tool_id: Some("fs.read".to_string()),
                bytes: 64,
            }),
            AgentWireEvent::SessionModel("gpt-4o".to_string()),
            AgentWireEvent::WorkspaceRootResolved(WorkspaceRootResolved {
                workspace_root: PathBuf::from("/tmp/some-workspace/.horizon/worktrees/abcd1234"),
                parent_session_id: Some(SessionId::new()),
            }),
        ];
        for event in events {
            let encoded = serde_json::to_value(&event).unwrap();
            let decoded: AgentWireEvent = serde_json::from_value(encoded).unwrap();
            assert_eq!(decoded, event);
        }
    }
}
