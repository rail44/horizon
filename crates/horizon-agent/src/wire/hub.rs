//! The agent runtime's hub — `horizon-agentd`'s whole rtc surface — and the
//! version pair it negotiates.
//!
//! Until `docs/runtime-crate-alignment-design.md` phase 2 this trait lived
//! in `horizon-session-protocol`, a union crate that also held the terminal
//! hub and the one `SESSION_PROTOCOL_VERSION` that spanned both wires.
//! Under the lockstep policy (no feature gates, mismatch → auto-drain-and-
//! respawn) that shared constant meant every agent-side wire change drained
//! the *terminal* runtime too, killing the PTYs the terminald split exists
//! to keep alive. Judgment 3 dissolved it: each hub now lives in its own
//! runtime's crate with its own version pair and its own schema artifact,
//! and the domain-free foundation both share stays in `horizon-wire`
//! ([`ClientHello`], [`VersionRange`], the codec pin, the size caps, and
//! [`HubError`]).
//!
//! Guardrail 1 (contract ≠ wire) is unchanged by the move: this module
//! references [`crate::contract`] and [`super`]'s vocabulary; nothing there
//! references this module. What the move *does* change is that remoc now
//! appears inside `horizon-agent` rather than only in a protocol crate —
//! the deliberate price of having the wire live with the runtime that owns
//! it (`docs/remoc-adoption-design.md` §1's exit-cost note still holds: the
//! vocabulary types themselves stay serde-plain, and remoc is confined to
//! this one module).
//!
//! Adoption conditions (binding, §1 of the design doc), as implemented
//! here and in the terminal hub's twin:
//!
//! 1. **The codec is pinned, never defaulted**: every channel field and
//!    every server/client construction names [`WireCodec`] (Postbag, Full
//!    configuration); the workspace disables remoc's `default-codec-*`
//!    features so `codec::Default` fails to compile if anything names it.
//!    Postbag is not self-describing, so the vocabularies' free-form JSON
//!    payloads (tool inputs/outputs) travel as
//!    [`crate::contract::JsonValue`] — their JSON text in one string —
//!    rather than `serde_json::Value`, whose `Deserialize` needs
//!    `deserialize_any`.
//! 2. **A non-final deserialization error is "skip this item," never "tear
//!    down the channel."** This project carries no cross-build wire
//!    compatibility: no wire enum has an
//!    `Unknown` catch-all left, so an unrecognized identifier and a
//!    structurally broken payload both surface as the same per-item decode
//!    error -- corruption to skip past, not a peer to tolerate.
//! 3. **`Connect::io` is polled on both ends concurrently** — in-process
//!    harnesses hosting both endpoints must `join!` the two handshakes
//!    (sequentially awaiting one side deadlocks and presents as a 60 s
//!    `ChMux(Timeout)`).

use remoc::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use horizon_wire::{
    channel_schema, CappedReceiver, ClientHello, HubError, VersionRange, WireCodec,
    CONTROL_MAX_ITEM_BYTES, TOOL_IO_MAX_ITEM_BYTES,
};

use super::{
    AgentWireEvent, HostToolRequest, HostToolResponse, ProviderSummary, SessionNew, SessionSummary,
};
use crate::contract::{Command, SessionId};

/// The agent-daemon protocol version this build speaks.
///
/// This constant and [`MIN_SUPPORTED_AGENT_PROTOCOL_VERSION`] are the agent
/// half of what was one shared pair through v18; the terminal half is
/// `horizon_terminal_core::wire::TERMINAL_PROTOCOL_VERSION`. Both halves
/// start at 18 — the split is a crate/artifact reorganization, not a wire
/// event — and from here on they move independently: an agent-side reshape
/// bumps only this one, and a running `horizon-terminald` keeps its PTYs
/// across it.
///
/// v4–v9 predate this: they versioned the old JSONL envelope wire, which
/// the v10 remoc cutover deleted wholesale, so no trace of that mechanism
/// survives to explain. From v10 on, one line each:
///
/// - **v10 — the remoc cutover** (`docs/remoc-adoption-design.md` §§2–3,
///   6): JSONL envelopes replaced by the [`SessionHub`] rtc trait plus
///   Postbag-encoded vocabularies over one remoc connection, `hello` first.
/// - **v11 — snapshot-valued frame path** (§5 Option A): the terminal
///   attachment's single updates channel split into a frame `rch::watch`
///   (full frame per delivery) and an events `rch::mpsc`; the wire diff
///   machinery was deleted wholesale.
/// - **v12 — scrollback windowed overscan negotiable**
///   (`docs/terminal-scrollback-design.md` §4): a pure feature-negotiation
///   signal — the wire surface itself landed additively in v11, no bump.
/// - **v13 — structured terminal input**
///   (`docs/terminal-kitty-associated-text-design.md` §3): `TerminalCommand`
///   gained `KeyInput`/`TextInput`, carrying the platform's associated text.
/// - **v14 — `MessageRole::TaskNotification`**
///   (`docs/agent-async-task-design.md` decision 2): a background `task`
///   child's completion is delivered as a message under its own role, so
///   the event log never records it as human-typed.
/// - **v15 — `Event::HistoryCleared`** (`docs/agent-compaction-design.md`
///   Tier 1): a compaction pass's frozen cleared-tool-result set became a
///   persisted, replayed event.
/// - **v16 — operator-intervention audit events** (`Event::
///   ApprovalResolved`, `Event::ContinueTurnRequested`): explicit event-log
///   records of a human's approve/deny and post-halt `ContinueTurn`,
///   closing gaps the 2026-07-28 dogfooding session aa95e066 surfaced.
/// - **v17 — the terminald split** (`docs/terminald-split-design.md`): the
///   single [`SessionHub`] became two hubs on two sockets; every surviving
///   agent method shifted index under Postbag, so the minimum floor rose
///   with it (the second such rise, after v11).
/// - **v18 — config-only `[provider]` reload**: `reload_provider_config`
///   appended to [`SessionHub`], letting `Reload Config` push a
///   model/base-URL change to a running `horizon-agentd` without a
///   respawn.
/// - **v19 — wire-only `Unknown` catch-alls removed; no decode compat**
///   (this is a personal project, so backward compatibility is not carried
///   by default).
/// - **v20 — board session routing and environment activation**: durable
///   session inputs/outcomes, in-session worktree handoff, and `watch_board`.
/// - **v21 — the multi-provider surface**: `list_providers` and
///   `set_session_model` appended to [`SessionHub`], plus
///   `Command::SetSessionModel` on the attachment's commands channel (the
///   owner-agreed multi-provider design's wire half). Method appends come
///   with a bump (the v18/v20 precedent): these are client→daemon behavior
///   the daemon must actually serve, so negotiated-version honesty wants
///   the honest restart — unlike a daemon→client event addition, where a
///   stale peer simply never sends it (the no-bump TaskProgress precedent
///   below).
/// - **v22 — live model discovery, and the model-list reshape**:
///   `list_provider_models` appended to [`SessionHub`] (a provider's own
///   `/models` listing for the picker), and `ProviderSummary` reshaped —
///   its `models` alias list replaced by `default_model`, since the
///   picker's candidates now come from the provider itself. The reshape
///   is why this is not a mere append: a stale peer must be drained and
///   respawned rather than decode a shape it never had.
///
/// - **v23 — required execution identity**: tool starts carry both IDs;
///   requests, approvals, decisions, and results require occurrence IDs.
///   Retired generic halt and sandbox-retry variants are removed. Restart
///   both shell and agentd; terminald keeps its independent protocol.
///
/// Additive since v19, no bump (the v12 precedent): **live background-task
/// progress** — `AgentWireEvent::TaskProgress` (`contract::TaskProgress`),
/// the daemon-side task watcher mirroring what a `task` child is doing
/// (current tool, reasoning vs tool-running) onto the requester's
/// attachment channel. Ephemeral UI feedback: never persisted, dropped
/// while no client is attached, not replayed on attach.
/// v24: tool results carry a typed outcome, including cancellation and the
/// replacement execution identity for superseded attempts.
/// v25 binds approve/deny commands to a ToolCallIdentity, including occurrence_id.
/// v26 requires keyed closure of streamed tool previews. Restart shell and agentd.
pub const AGENT_PROTOCOL_VERSION: u32 = 27;

/// The oldest agent-wire version this build is still willing to negotiate
/// down to in [`SessionHub::hello`] — the low end of the advertised
/// `[min_supported, current]` range. Equal to [`AGENT_PROTOCOL_VERSION`]
/// under the standing **lockstep, no per-feature gates** policy (owner,
/// 2026-07-30): same-machine self-spawned daemons do not need cross-version
/// interop, they need honest restart, so a mismatched `hello` is rejected
/// and recovered by the client's auto-drain-and-respawn (`docs/remoc-
/// adoption-design.md` §3/§6) rather than bridged by gate constants.
pub const MIN_SUPPORTED_AGENT_PROTOCOL_VERSION: u32 = 27;

/// The version range this build advertises in every `hello` to
/// `horizon-agentd`.
///
/// The range *type* is domain-free ([`VersionRange`]); which numbers go in
/// it is not, which is why this constructor sits beside the agent hub's own
/// two constants rather than on the type. `horizon_terminal_core::wire::
/// terminal_version_range` is its terminal-side twin.
pub fn agent_version_range() -> VersionRange {
    VersionRange::new(MIN_SUPPORTED_AGENT_PROTOCOL_VERSION, AGENT_PROTOCOL_VERSION)
}

/// A [`ClientHello`] advertising [`agent_version_range`] under `binary_id` —
/// what every client sends as the first call on a connection to
/// `horizon-agentd`.
pub fn agent_client_hello(binary_id: impl Into<String>) -> ClientHello {
    ClientHello::new(agent_version_range(), binary_id)
}

/// `horizon-agentd`'s `hello` reply: the negotiated version plus the
/// connection-global channels (`docs/remoc-adoption-design.md` §2 — what
/// used to be connection-global envelope kinds now rides channels handed
/// over here; everything session-scoped rides the per-attachment channels
/// instead). Every channel here is agent-domain, which is why
/// [`TerminalHubHello`] carries none of them.
//
// The `[`TerminalHubHello`]` link above dangles on purpose: that type is
// `horizon-terminal-core`'s, a crate this one must never depend on. The
// wording is pinned byte-for-byte by the committed wire-schema artifact
// (it is this type's `description`), so it stays exactly as written —
// the same rule `horizon_wire::negotiate` records for its own types.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct HubHello {
    /// The highest mutually supported version — the version this
    /// connection's *behavior* may rely on (§3: gates behavior, not
    /// decodability).
    pub negotiated: u32,
    pub binary_id: String,
    /// Daemon → client: a hosted session asking the client to run a
    /// host-coupled tool (e.g. `workspace.snapshot`). Replaces the
    /// connection-global `host_tool_request` envelopes.
    #[schemars(schema_with = "channel_schema::<HostToolRequest>")]
    pub host_tools: CappedReceiver<HostToolRequest, TOOL_IO_MAX_ITEM_BYTES>,
    /// Client → daemon: the answers to `host_tools` requests, correlated by
    /// `request_id` exactly as before (the one correlation map the cutover
    /// keeps: the exchange is genuinely asynchronous on the daemon side,
    /// where a session thread blocks on the matching response).
    #[schemars(schema_with = "channel_schema::<HostToolResponse>")]
    pub host_tool_responses: rch::mpsc::Sender<HostToolResponse, WireCodec>,
    /// Daemon → client: the daemon's startup event-log corruption summary,
    /// sent at most once per connection, after its resume finishes.
    /// Replaces the `SkippedLines` control envelope.
    #[schemars(schema_with = "channel_schema::<String>")]
    pub skipped_lines: CappedReceiver<String, CONTROL_MAX_ITEM_BYTES>,
}

/// What [`SessionHub::new_agent`]/[`SessionHub::attach_agent`] hand back.
/// `events` carries both the session's provider events and the
/// session-scoped announcements that used to be their own control
/// envelopes (`SessionModel`, `ToolCallProgress`,
/// `WorkspaceRootResolved`) — see
/// [`horizon_agent::wire::AgentWireEvent`].
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct AgentAttachment {
    #[schemars(schema_with = "channel_schema::<AgentWireEvent>")]
    pub events: CappedReceiver<AgentWireEvent, TOOL_IO_MAX_ITEM_BYTES>,
    #[schemars(schema_with = "channel_schema::<Command>")]
    pub commands: rch::mpsc::Sender<Command, WireCodec>,
}

/// The agent session hub — `horizon-agentd`'s rtc surface
/// (`docs/remoc-adoption-design.md` §2). The daemon serves it over its unix
/// socket; [`hello`](Self::hello) must be the first call on every
/// connection. `hello` and [`drain`](Self::drain) are the version-stable
/// surface (the conversations that must keep working across future protocol
/// versions, like the JSONL era's `session_control` kind); everything else
/// may evolve additively under the §4 skew discipline.
///
/// Terminal hosting left this trait in v17 — see
/// `horizon_terminal_core::wire::TerminalHub`.
#[rtc::remote]
pub trait SessionHub {
    /// Version negotiation (`docs/remoc-adoption-design.md` §3): the first
    /// call on every connection. Replaces the exact-match JSONL handshake
    /// with `[min_supported, current]` range intersection.
    async fn hello(&self, client: ClientHello) -> Result<HubHello, HubError>;

    /// Every live agent session. Replaces
    /// `Control::SessionList`/`SessionListResult`.
    async fn list_agents(&self) -> Result<Vec<SessionSummary>, HubError>;

    /// Registers a project's board for task/session event delivery.
    async fn watch_board(&self, workspace_root: std::path::PathBuf) -> Result<(), HubError>;

    /// Spawns a fresh agent session (`Control::SessionNew`) and attaches
    /// to it.
    async fn new_agent(&self, new: SessionNew) -> Result<AgentAttachment, HubError>;

    /// Attaches to an existing agent session (`Control::SessionLoad`): the
    /// returned attachment's `events` channel replays the session's
    /// committed events first (followed by its resolved model, if any),
    /// then carries live events. An unknown session id succeeds with an
    /// empty replay, exactly as `session_load` did.
    async fn attach_agent(&self, session_id: SessionId) -> Result<AgentAttachment, HubError>;

    // -- lifecycle --

    /// Flush-and-exit, replacing `SessionControl::Drain`: the daemon
    /// flushes its event log to disk and exits. The call itself typically
    /// errors (the process is gone before a reply can travel); callers
    /// observe completion as the socket refusing connections, same as
    /// before. Since v17 this no longer touches a single PTY — terminals
    /// belong to `horizon-terminald`, whose own
    /// `TerminalHub::drain` is the destructive counterpart.
    async fn drain(&self) -> Result<(), HubError>;

    /// Re-reads `[provider]` from the config file and rebuilds the
    /// provider registry in place -- the no-respawn counterpart to
    /// `Reload Agent Runtime` for a config-only model/base-URL change. A
    /// running session keeps its spawn-time config for its whole
    /// lifetime, so the new provider takes effect for the *next* session.
    async fn reload_provider_config(&self) -> Result<(), HubError>;

    /// Creates or resumes the project organizer without submitting work.
    async fn ensure_board_organizer(
        &self,
        workspace_root: std::path::PathBuf,
    ) -> Result<SessionId, HubError>;

    // -- provider surface (v21, the multi-provider design's wire half) --

    /// Every configured provider with its default model and availability —
    /// the model picker's data. Entries run in the config file's order.
    /// `available` is build-time resolved: an entry whose API-key variable
    /// was unset registers but is unavailable — grayed out, never hidden.
    /// The entry's candidate models are its own live `/models` listing
    /// ([`Self::list_provider_models`]), not the file.
    async fn list_providers(&self) -> Result<Vec<ProviderSummary>, HubError>;

    /// Mid-session provider/model switch, latest turn wins: the *next*
    /// turn runs on `provider`/`model`, whatever is in flight finishes on
    /// its own selection. `provider` names a configured entry (see
    /// [`Self::list_providers`]); `model` is a model id — typically one the
    /// provider's `/models` listed, the same pass-through `role.model`
    /// accepts. Validates against current config and queues a resolved snapshot.
    /// Success means acceptance; the provider announces the model only after
    /// applying it. Reattach reports the last applied selection.
    /// Unknown provider, unknown session, or an empty
    /// model is a [`HubError`].
    async fn set_session_model(
        &self,
        session_id: SessionId,
        provider: String,
        model: String,
    ) -> Result<(), HubError>;

    /// A provider's own live model listing (`GET {base_url}/models`) — the
    /// ids the model picker offers (v22). `provider` names a configured
    /// entry. An entry with no usable key, a provider that answers no
    /// listing, or a transport failure yields an empty list — discovery
    /// never blocks a pick, so "nothing discovered" is not an error to the
    /// caller.
    async fn list_provider_models(&self, provider: String) -> Result<Vec<String>, HubError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_range_negotiates_with_itself_at_the_current_version() {
        assert_eq!(
            agent_version_range().negotiate(agent_version_range()),
            Some(AGENT_PROTOCOL_VERSION)
        );
    }

    /// Board routing and environment handoff require matching shell/agentd
    /// builds. The terminal protocol evolves independently.
    #[test]
    fn board_routing_requires_the_current_agent_protocol() {
        assert_eq!(AGENT_PROTOCOL_VERSION, 27);
        assert_eq!(MIN_SUPPORTED_AGENT_PROTOCOL_VERSION, AGENT_PROTOCOL_VERSION);
    }

    /// The hub *method surface*, snapshotted mechanically from the serde
    /// shape of the rtc macro's generated request enum (`SessionHubReqRef`
    /// — every `&self` method becomes one variant whose fields are the
    /// method's arguments). The artifact's `hub` section is hand-written
    /// prose; this test is the machine check behind it: renaming a
    /// method or an argument changes these serde error strings and goes
    /// red, so the artifact cannot silently drift from the real trait.
    #[test]
    fn hub_request_enum_matches_the_documented_method_surface() {
        // Variant list = method list, from serde's unknown-variant error.
        let variants =
            match serde_json::from_str::<SessionHubReqRef<WireCodec>>("{\"__bogus\":null}") {
                Ok(_) => panic!("a bogus variant must fail"),
                Err(error) => error.to_string(),
            };
        assert_eq!(
            variants,
            "unknown variant `__bogus`, expected one of `Hello`, `ListAgents`, `WatchBoard`, `NewAgent`, \
             `AttachAgent`, `Drain`, `ReloadProviderConfig`, `EnsureBoardOrganizer`, `ListProviders`, \
             `SetSessionModel`, `ListProviderModels` at line 1 column 10",
        );

        // Argument names per method, from serde's missing-field errors.
        // The macro declares its own reply channel (`__reply_tx`) as the
        // variant's first field, so the probe satisfies it with the
        // "closed sender" transported shape (`port: null` needs no
        // connection context) — the next missing field serde reports is
        // then the method's first argument.
        let probe = |method: &str| {
            let json = format!(
                "{{\"{method}\": {{\"__reply_tx\": {{\"port\": null, \"data\": null, \
                 \"codec\": null}}}}}}"
            );
            match serde_json::from_str::<SessionHubReqRef<WireCodec>>(&json) {
                Ok(_) => format!("{method}: no further required fields"),
                Err(error) => error.to_string(),
            }
        };
        assert!(
            probe("Hello").starts_with("missing field `client`"),
            "{}",
            probe("Hello")
        );
        assert!(
            probe("NewAgent").starts_with("missing field `new`"),
            "{}",
            probe("NewAgent")
        );
        assert!(
            probe("EnsureBoardOrganizer").starts_with("missing field `workspace_root`"),
            "{}",
            probe("EnsureBoardOrganizer")
        );
        assert!(
            probe("AttachAgent").starts_with("missing field `session_id`"),
            "{}",
            probe("AttachAgent")
        );
        // v21's additions: list_providers takes no arguments, set_session_model
        // carries the session id then the provider/model pair.
        assert_eq!(
            probe("ListProviders"),
            "ListProviders: no further required fields"
        );
        assert!(
            probe("SetSessionModel").starts_with("missing field `session_id`"),
            "{}",
            probe("SetSessionModel")
        );
        // v22's addition: list_provider_models carries the provider name.
        assert!(
            probe("ListProviderModels").starts_with("missing field `provider`"),
            "{}",
            probe("ListProviderModels")
        );
    }
}
