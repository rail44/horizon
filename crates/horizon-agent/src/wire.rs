//! The JSONL wire envelope for `horizon-sessiond`'s socket protocol -- see
//! `docs/agent-runtime-split-design.md`'s decision 4 and ACP guardrails 1-2.
//!
//! **Guardrail 1 (contract ≠ wire)**: this module references
//! [`crate::contract`] types (`Command`, `Event`, `SessionId`, ...) to build
//! the envelope shape; nothing in `contract` references this module. An ACP
//! adapter is a second binding beside this one, translating JSON-RPC to the
//! same contract types.
//!
//! **Guardrail 2 (framing over any stream)**: [`read_envelope`]/
//! [`write_envelope`] are generic over `tokio::io::{AsyncBufRead,
//! AsyncWrite}` -- nothing here names `UnixStream` or any other concrete
//! transport. Callers (`horizon-sessiond`'s connection handler, Horizon's
//! `agent::connection`) wrap whatever socket/pipe they have (typically
//! `tokio::io::BufReader::new(unix_stream_read_half)` for the read side)
//! and pass it in here.

use std::path::PathBuf;

use horizon_session_protocol::Envelope as ProtocolEnvelope;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncWrite};

use crate::contract::{Command, Event, ProviderId, RequestId, SessionId, ToolCallProgress};
use crate::roles::RoleId;

/// Shared handshake types, errors, and the contract/wire version this build
/// speaks. The version is carried by every envelope and independently by
/// [`Hello::contract_version`]. Version 3 introduces qualified sister-domain
/// kinds and moves shared lifecycle controls into `horizon-session-protocol`.
///
/// The definitions now live in `horizon-session-protocol`; these re-exports
/// preserve this module's public API while terminal and agent messages share
/// one session-daemon connection.
pub use horizon_session_protocol::{
    Hello, WireError, SESSION_PROTOCOL_VERSION as CONTRACT_VERSION,
};

pub(crate) const AGENT_CONTROL_KIND: &str = "agent_control";
pub(crate) const AGENT_COMMAND_KIND: &str = "agent_command";
pub(crate) const AGENT_EVENT_KIND: &str = "agent_event";

/// One JSONL agent-domain message. `session_id` is `None` for
/// connection-global agent controls such as `session_list` and `Some`
/// for anything scoped to one agent session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Envelope {
    pub v: u32,
    pub session_id: Option<SessionId>,
    #[serde(flatten)]
    pub body: EnvelopeBody,
}

impl Envelope {
    pub fn command(session_id: SessionId, command: Command) -> Self {
        Self {
            v: CONTRACT_VERSION,
            session_id: Some(session_id),
            body: EnvelopeBody::Command(command),
        }
    }

    pub fn event(session_id: SessionId, event: Event) -> Self {
        Self {
            v: CONTRACT_VERSION,
            session_id: Some(session_id),
            body: EnvelopeBody::Event(event),
        }
    }

    /// A connection-global control message (`session_id: None`). Construct
    /// [`Envelope`] directly (struct literal -- every field is `pub`) for a
    /// control message scoped to one session, e.g. a future session-bound
    /// host-tool exchange.
    pub fn control(control: Control) -> Self {
        Self {
            v: CONTRACT_VERSION,
            session_id: None,
            body: EnvelopeBody::Control(control),
        }
    }
}

/// The envelope's `kind`/`payload` pair. Serializes adjacently tagged
/// (`{"kind":"agent_command","payload":{..}}`) through the raw-envelope
/// adapter; deserializing
/// needs the version check *before* picking which contract type to decode
/// `payload` as, so [`read_envelope`] performs the shared structural read
/// before decoding this enum -- see [`WireError::UnknownKind`]/
/// [`WireError::VersionMismatch`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum EnvelopeBody {
    Command(Command),
    Event(Event),
    Control(Control),
}

/// Agent-domain control messages. Shared connection lifecycle controls live
/// in `horizon_session_protocol::SessionControl`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    SessionList,
    SessionListResult(Vec<SessionSummary>),
    SessionNew(SessionNew),
    SessionLoad(SessionLoad),
    HostToolRequest(HostToolRequest),
    HostToolResponse(HostToolResponse),
    /// Ephemeral tool-call-argument-streaming preview
    /// (`contract::ProviderEvent::tool_call_progress`), session-scoped via
    /// the envelope's own `session_id` (not this payload). `contract::Event`
    /// deliberately has no variant for this -- it's UI-only feedback, never
    /// part of conversation history and never persisted (see
    /// [`ToolCallProgress`]'s own doc comment) -- so a step-3 pass filtered
    /// it out entirely rather than invent a wire `Event` representation for
    /// something the contract itself excludes. Restored here as its own
    /// control message instead: reuses `contract::ToolCallProgress` as-is
    /// (guardrail 1 is about the *direction* `wire` -> `contract` already
    /// established by every other `Control` payload, not about which module
    /// owns a struct). See `horizon-sessiond`'s `session::handle_provider_event`
    /// (sender) and `horizon`'s `agent::connection::dispatch_incoming`
    /// (receiver, which folds it through the exact same
    /// `apply_tool_call_progress_to_frame` path a persisted event would).
    ToolCallProgress(ToolCallProgress),
    /// A session's resolved model id, session-scoped via the envelope's own
    /// `session_id` -- same shape of exception as [`Control::ToolCallProgress`]
    /// just above: `contract::ProviderEvent::session_model` is ephemeral
    /// (never part of conversation history, never persisted), so it travels
    /// as its own `Control` rather than a new `contract::Event` variant. Sent
    /// once by `horizon-sessiond`, either right after a fresh
    /// `Control::SessionNew` resolves its session's model, or alongside a
    /// `Control::SessionLoad`'s replayed events (so a (re)attaching client
    /// gets it too, not just the client present at session start) -- see
    /// `docs/agent-output-ui-amendment.md`'s dated model-chip addendum.
    /// Omitted entirely when the provider has no resolvable model (mirrors
    /// [`Control::SkippedLines`]'s "just don't send it" convention below).
    SessionModel(String),
    /// This process's own startup event-log corruption diagnostics
    /// (`persistence::event_log::ReadReport::skipped_summary`), sent once
    /// per connection -- after `horizon-sessiond`'s startup resume finishes,
    /// never blocking `Hello`'s immediate reply the way answering inside
    /// `Hello` itself would (see `horizon-sessiond::main`'s bind-first
    /// ordering doc comment; `session_list`/`session_load` already block on
    /// the same readiness gate for the same reason). Connection-global, like
    /// [`Control::SessionList`] -- omitted entirely when nothing was
    /// skipped. See `horizon-sessiond`'s `main::run_session_hosting_loop`
    /// (sender) and `horizon`'s `agent::connection::dispatch_incoming`
    /// (receiver, which folds it into `agent_state_status`, the status
    /// bar's existing signal).
    SkippedLines(String),
    /// Live correction of a freshly isolated session's authoritative
    /// `workspace_root` (and, since the same resolution moment also decides
    /// the derivation edge, `parent_session_id`) -- session-scoped via the
    /// envelope's own `session_id`, same shape of exception as
    /// [`Control::SessionModel`] above. Sent once by `horizon-sessiond`,
    /// right after `session::resolve_and_create_isolated_worktree` resolves
    /// this session's isolated worktree: worktree creation is real IO that
    /// only finishes *after* `Control::SessionNew` already returned, so a
    /// fresh spawn's shell-side `workspace_root` is only ever the
    /// pre-isolation guess until now. Closes the last "still eventual, not
    /// live" gap `docs/session-relationship-design.md`'s delivery notes call
    /// out: without this, a session created and used within one continuous
    /// run only saw its corrected root/parent via a later
    /// `spawn_agent_resume`/`spawn_workspace_restore` sweep (daemon restart
    /// or UI restart). Not sent at all when isolation fails and degrades to
    /// a shared spawn -- there is nothing to correct then, mirroring
    /// [`SessionSummary::parent_session_id`]'s "the edge exists only via
    /// isolation".
    WorkspaceRootResolved(WorkspaceRootResolved),
}

/// [`Control::WorkspaceRootResolved`]'s payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceRootResolved {
    pub workspace_root: PathBuf,
    /// Additive, like [`SessionSummary::parent_session_id`] -- `None` for an
    /// isolated-but-sourceless spawn (still a valid lineage root, see that
    /// field's own doc comment).
    #[serde(default)]
    pub parent_session_id: Option<SessionId>,
}

/// One entry of a [`Control::SessionListResult`] reply.
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
    /// was given. `#[serde(default)]`, purely additive like `SessionNew.
    /// workspace_root` below -- no `CONTRACT_VERSION` bump. A resumed
    /// session (`session::resume_persisted_sessions`) always reports
    /// `None` here today: lineage lives in `horizon-sessiond`'s in-memory
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
    /// `Control::SessionNew` already returned -- see
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

/// Per `docs/agent-runtime-split-design.md` guardrail 5, `session_new` is
/// distinct from `session_load` and carries per-session overrides.
/// `role_id` replaces this field's former placeholder shape
/// (`config_overrides: Option<serde_json::Value>`) -- see
/// [`CONTRACT_VERSION`]'s doc comment for why that was a breaking, not
/// additive, change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionNew {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
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
    /// precedent rather than [`CONTRACT_VERSION`]'s breaking-change one.
    /// Passed into `SessiondHandle::start_session` by the workspace layer
    /// (`WorkspaceShell::reconcile`), which computes the Horizon process's
    /// own cwd once per session (falling back to `None` only if that cwd
    /// can't be read) and records the same value on the session's
    /// `WorkspaceSession::workspace_root` -- so a session's workspace root
    /// tracks whichever Horizon window spawned it, not `horizon-sessiond`'s
    /// own cwd (one shared, long-lived daemon per user, started from
    /// whatever directory happened to be current the first time it was
    /// launched).
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionLoad {
    pub session_id: SessionId,
}

/// The agent (child) asking the client to run a host-coupled tool (e.g.
/// `workspace.snapshot`) over this same connection -- guardrail 4. Not yet
/// sent or handled anywhere in step 2 (tool execution stays in Horizon
/// until step 3); the shape exists here so the wire format is settled.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostToolRequest {
    pub request_id: RequestId,
    pub tool_id: String,
    pub input: serde_json::Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostToolResponse {
    pub request_id: RequestId,
    pub output: serde_json::Value,
}

/// Writes one envelope as a single newline-terminated JSON line and flushes
/// the writer, so a peer reading line-by-line (e.g. [`read_envelope`]) sees
/// it immediately rather than waiting on a fuller buffer.
pub fn encode_envelope(envelope: &Envelope) -> Result<ProtocolEnvelope, WireError> {
    let (kind, payload) = match &envelope.body {
        EnvelopeBody::Command(command) => (AGENT_COMMAND_KIND, serde_json::to_value(command)?),
        EnvelopeBody::Event(event) => (AGENT_EVENT_KIND, serde_json::to_value(event)?),
        EnvelopeBody::Control(control) => (AGENT_CONTROL_KIND, serde_json::to_value(control)?),
    };
    Ok(ProtocolEnvelope {
        v: envelope.v,
        session_id: envelope.session_id.map(SessionId::as_uuid),
        kind: kind.to_string(),
        payload,
    })
}

pub fn decode_envelope(raw: ProtocolEnvelope) -> Result<Envelope, WireError> {
    let body = match raw.kind.as_str() {
        AGENT_COMMAND_KIND => EnvelopeBody::Command(serde_json::from_value(raw.payload)?),
        AGENT_EVENT_KIND => EnvelopeBody::Event(serde_json::from_value(raw.payload)?),
        AGENT_CONTROL_KIND => EnvelopeBody::Control(serde_json::from_value(raw.payload)?),
        other => return Err(WireError::UnknownKind(other.to_string())),
    };
    Ok(Envelope {
        v: raw.v,
        session_id: raw.session_id.map(SessionId::from_uuid),
        body,
    })
}

pub async fn write_envelope<W>(writer: &mut W, envelope: &Envelope) -> Result<(), WireError>
where
    W: AsyncWrite + Unpin,
{
    let protocol_envelope = encode_envelope(envelope)?;
    horizon_session_protocol::write_envelope(writer, &protocol_envelope).await
}

/// Reads one newline-delimited envelope. `Ok(None)` means the peer closed
/// the connection cleanly between messages (0 bytes read, no partial line
/// pending); a partial line with no trailing newline (peer closed
/// mid-message) is [`WireError::TornLine`], never silently treated as a
/// complete (truncated) message.
pub async fn read_envelope<R>(reader: &mut R) -> Result<Option<Envelope>, WireError>
where
    R: AsyncBufRead + Unpin,
{
    let Some(raw) = horizon_session_protocol::read_envelope(reader).await? else {
        return Ok(None);
    };
    Ok(Some(decode_envelope(raw)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ToolCallId;
    use tokio::io::{AsyncWriteExt, BufReader};

    fn sample_command() -> Command {
        Command::UserMessage {
            text: "hi".to_string(),
        }
    }

    fn sample_event() -> Event {
        Event::ToolCallRequested(crate::contract::ToolCallRequest {
            call_id: ToolCallId("call-1".to_string()),
            tool_id: "fs.read".to_string(),
            input: serde_json::json!({"path": "a.txt"}),
        })
    }

    fn sample_controls() -> Vec<Control> {
        vec![
            Control::SessionList,
            Control::SessionListResult(vec![SessionSummary {
                session_id: SessionId::new(),
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: Some(RoleId("config".to_string())),
                parent_session_id: Some(SessionId::new()),
                workspace_root: Some(PathBuf::from("/tmp/some-workspace")),
            }]),
            Control::SessionNew(SessionNew {
                session_id: SessionId::new(),
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: Some(RoleId("config".to_string())),
                workspace_root: Some(PathBuf::from("/tmp/some-workspace")),
                spawn_source_session_id: Some(SessionId::new()),
                isolate: true,
            }),
            Control::SessionLoad(SessionLoad {
                session_id: SessionId::new(),
            }),
            Control::HostToolRequest(HostToolRequest {
                request_id: RequestId("req-1".to_string()),
                tool_id: "workspace.snapshot".to_string(),
                input: serde_json::json!({}),
            }),
            Control::HostToolResponse(HostToolResponse {
                request_id: RequestId("req-1".to_string()),
                output: serde_json::json!({"ok": true}),
            }),
            Control::ToolCallProgress(ToolCallProgress {
                key: "call-1".to_string(),
                tool_id: Some("fs.read".to_string()),
                bytes: 64,
            }),
            Control::SessionModel("gpt-4o".to_string()),
            Control::SkippedLines("skipped 1 corrupt line".to_string()),
            Control::WorkspaceRootResolved(WorkspaceRootResolved {
                workspace_root: PathBuf::from("/tmp/some-workspace/.horizon/worktrees/abcd1234"),
                parent_session_id: Some(SessionId::new()),
            }),
        ]
    }

    /// The shared contract has since advanced past the breaking terminal
    /// discovery/attach vocabulary introduced alongside session recovery
    /// (v4); v5 is the terminal frame's move to the Horizon-owned color
    /// vocabulary, v6 is the removal of `Hello`'s dead `capabilities`
    /// field (2026-07-18), and v7 is the frame vocabulary's style bits /
    /// semantic selection / cursor shape extension (2026-07-19).
    #[test]
    fn contract_version_includes_correlated_terminal_recovery() {
        assert_eq!(CONTRACT_VERSION, 7);
    }

    /// Unlike `role_id` above, `SessionNew.workspace_root` is a brand-new,
    /// `#[serde(default)]` field, not a reshape of an existing one -- see
    /// its own doc comment. Additive changes don't need a version bump;
    /// the current value reflects later breaking changes in the terminal
    /// domain's vocabulary (v4 discovery/attach, v5 owned colors, v6
    /// dropped `Hello.capabilities`, v7 frame styles/selection/cursor
    /// shape).
    #[test]
    fn contract_version_was_not_bumped_for_the_additive_workspace_root_field() {
        assert_eq!(CONTRACT_VERSION, 7);
    }

    /// Same rule as `workspace_root` above, for the two session-relationship
    /// fields (`docs/session-relationship-design.md` decision 3):
    /// `SessionNew.spawn_source_session_id`/`isolate` and `SessionSummary.
    /// parent_session_id` are all brand-new `#[serde(default)]` fields, not
    /// reshapes of existing ones -- no version bump needed.
    #[test]
    fn contract_version_was_not_bumped_for_the_additive_lineage_fields() {
        assert_eq!(CONTRACT_VERSION, 7);
    }

    /// A brand-new `Control` variant is exactly as additive as a new field
    /// on an existing one, per the same rule -- `Control::
    /// WorkspaceRootResolved` follows `Control::SessionModel`'s own
    /// precedent of adding a session-scoped announcement variant without a
    /// version bump.
    #[test]
    fn contract_version_was_not_bumped_for_the_workspace_root_resolved_announcement() {
        assert_eq!(CONTRACT_VERSION, 7);
    }

    /// A `session_new` control message written by a peer built before
    /// `workspace_root` existed has no such key in its JSON payload at all
    /// -- `#[serde(default)]` must still parse it (as `None`), not reject
    /// the envelope. Mirrors `persistence::event_log::Record`'s
    /// `reads_a_pre_role_record_with_no_role_id_key` regression guard.
    #[tokio::test]
    async fn session_new_without_a_workspace_root_key_deserializes_to_none() {
        let (mut client, server) = tokio::io::duplex(1024);
        let mut server_reader = BufReader::new(server);

        let session_id = SessionId::new();
        let line = serde_json::json!({
            "v": CONTRACT_VERSION,
            "session_id": null,
            "kind": AGENT_CONTROL_KIND,
            "payload": {
                "session_new": {
                    "session_id": session_id,
                    "provider_id": "builtin.agent.rig",
                    "role_id": null,
                }
            }
        })
        .to_string();
        client
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        drop(client);

        let envelope = read_envelope(&mut server_reader)
            .await
            .unwrap()
            .expect("envelope should parse despite the missing workspace_root key");
        match envelope.body {
            EnvelopeBody::Control(Control::SessionNew(new)) => {
                assert_eq!(new.workspace_root, None);
            }
            other => panic!("expected Control::SessionNew, got {other:?}"),
        }
    }

    /// Same regression guard as the `workspace_root` test above, for the two
    /// fields added alongside the session-relationship model: a peer built
    /// before they existed sends a `session_new` payload with neither key at
    /// all, and it must still parse, defaulting to "no source, not
    /// isolated" (today's only behavior before this pair existed).
    #[tokio::test]
    async fn session_new_without_lineage_keys_deserializes_to_no_source_and_not_isolated() {
        let (mut client, server) = tokio::io::duplex(1024);
        let mut server_reader = BufReader::new(server);

        let session_id = SessionId::new();
        let line = serde_json::json!({
            "v": CONTRACT_VERSION,
            "session_id": null,
            "kind": AGENT_CONTROL_KIND,
            "payload": {
                "session_new": {
                    "session_id": session_id,
                    "provider_id": "builtin.agent.rig",
                    "role_id": null,
                }
            }
        })
        .to_string();
        client
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        drop(client);

        let envelope = read_envelope(&mut server_reader)
            .await
            .unwrap()
            .expect("envelope should parse despite the missing lineage keys");
        match envelope.body {
            EnvelopeBody::Control(Control::SessionNew(new)) => {
                assert_eq!(new.spawn_source_session_id, None);
                assert!(!new.isolate);
            }
            other => panic!("expected Control::SessionNew, got {other:?}"),
        }
    }

    /// Same for `SessionSummary.parent_session_id` on the *reply* side: a
    /// `session_list_result` entry from a peer built before this field
    /// existed has no such key, and must parse as `None`, not reject the
    /// envelope.
    #[tokio::test]
    async fn session_summary_without_a_parent_session_id_key_deserializes_to_none() {
        let (mut client, server) = tokio::io::duplex(1024);
        let mut server_reader = BufReader::new(server);

        let session_id = SessionId::new();
        let line = serde_json::json!({
            "v": CONTRACT_VERSION,
            "session_id": null,
            "kind": AGENT_CONTROL_KIND,
            "payload": {
                "session_list_result": [{
                    "session_id": session_id,
                    "provider_id": "builtin.agent.rig",
                    "role_id": null,
                }]
            }
        })
        .to_string();
        client
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        drop(client);

        let envelope = read_envelope(&mut server_reader)
            .await
            .unwrap()
            .expect("envelope should parse despite the missing parent_session_id key");
        match envelope.body {
            EnvelopeBody::Control(Control::SessionListResult(summaries)) => {
                assert_eq!(summaries.len(), 1);
                assert_eq!(summaries[0].parent_session_id, None);
            }
            other => panic!("expected Control::SessionListResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn round_trips_every_envelope_kind_over_a_duplex_stream() {
        let session_id = SessionId::new();
        let mut envelopes = vec![
            Envelope::command(session_id, sample_command()),
            Envelope::command(session_id, Command::ContinueTurn),
            Envelope::event(session_id, sample_event()),
        ];
        envelopes.extend(sample_controls().into_iter().map(Envelope::control));

        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let mut server_reader = BufReader::new(server);

        for envelope in &envelopes {
            write_envelope(&mut client, envelope).await.unwrap();
        }
        drop(client);

        let mut received = Vec::new();
        while let Some(envelope) = read_envelope(&mut server_reader).await.unwrap() {
            received.push(envelope);
        }

        assert_eq!(received, envelopes);
    }

    #[tokio::test]
    async fn torn_line_without_trailing_newline_is_an_explicit_error() {
        let (mut client, server) = tokio::io::duplex(1024);
        let mut server_reader = BufReader::new(server);

        client
            .write_all(b"{\"v\":1,\"kind\":\"control\"")
            .await
            .unwrap();
        drop(client);

        let result = read_envelope(&mut server_reader).await;
        assert!(matches!(result, Err(WireError::TornLine)), "{result:?}");
    }

    #[tokio::test]
    async fn clean_disconnect_between_messages_is_not_an_error() {
        let (client, server) = tokio::io::duplex(1024);
        let mut server_reader = BufReader::new(server);
        drop(client);

        let result = read_envelope(&mut server_reader).await;
        assert!(matches!(result, Ok(None)), "{result:?}");
    }

    #[tokio::test]
    async fn unknown_kind_is_an_explicit_error_not_a_panic() {
        let (mut client, server) = tokio::io::duplex(1024);
        let mut server_reader = BufReader::new(server);

        client
            .write_all(
                format!(
                    "{{\"v\":{CONTRACT_VERSION},\"kind\":\"bogus\",\"session_id\":null,\"payload\":{{}}}}\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        drop(client);

        let result = read_envelope(&mut server_reader).await;
        match result {
            Err(WireError::UnknownKind(kind)) => assert_eq!(kind, "bogus"),
            other => panic!("expected UnknownKind, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wire_version_mismatch_is_an_explicit_error() {
        let (mut client, server) = tokio::io::duplex(1024);
        let mut server_reader = BufReader::new(server);

        client
            .write_all(
                b"{\"v\":99,\"kind\":\"control\",\"session_id\":null,\"payload\":\"ping\"}\n",
            )
            .await
            .unwrap();
        drop(client);

        let result = read_envelope(&mut server_reader).await;
        assert!(
            matches!(
                result,
                Err(WireError::VersionMismatch {
                    expected: CONTRACT_VERSION,
                    found: 99
                })
            ),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn unknown_top_level_fields_are_tolerated() {
        let (mut client, server) = tokio::io::duplex(1024);
        let mut server_reader = BufReader::new(server);

        client
            .write_all(
                format!(
                    "{{\"v\":{CONTRACT_VERSION},\"kind\":\"agent_control\",\"session_id\":null,\
                     \"payload\":\"session_list\",\"future_field\":42}}\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        drop(client);

        let envelope = read_envelope(&mut server_reader)
            .await
            .unwrap()
            .expect("envelope should parse despite the unrecognized field");
        assert_eq!(envelope.body, EnvelopeBody::Control(Control::SessionList));
    }
}
