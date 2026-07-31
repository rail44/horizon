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
//! 2. **Every wire enum carries a `#[serde(other)] Unknown` catch-all**,
//!    and receive loops treat a non-final deserialization error as "skip
//!    this item", never "tear down the channel".
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

use super::{AgentWireEvent, HostToolRequest, HostToolResponse, SessionNew, SessionSummary};
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
/// The numbered history below is the *shared* v4–v18 history of the single
/// pre-split version, kept whole here rather than duplicated or divided:
/// several bumps (v5, v7, v8, v9, v11) were terminal-side changes that
/// nonetheless moved this number, so splitting the narrative by domain
/// would rewrite what actually happened.
/// `horizon_terminal_core::wire::TERMINAL_PROTOCOL_VERSION` points back
/// here for it.
///
/// Version 4 adds correlated terminal discovery and attach controls; attach
/// changed shape, so older peers cannot safely decode the terminal vocabulary.
///
/// Version 5: `TerminalSpan`'s `fg`/`bg` now carry `horizon-terminal-core`'s
/// own `TerminalColor`/`NamedColor` enums instead of a re-exported
/// `alacritty_terminal::vte::ansi::Color`/`NamedColor` — same role, different
/// wire shape (variant names/order changed, `Spec(Rgb)` became `Rgb([u8;
/// 3])`), so a stale daemon/UI pair must fail the handshake rather than
/// misdecode a frame's colors.
///
/// Version 6: `Hello` drops the dead `capabilities` field (owner decision,
/// 2026-07-18) -- every sender hardcoded `["agent", "terminal"]` and the
/// only reader was a test assertion, so it was forward-compat weight with
/// no actual use. Removing a field changes the wire shape, so a stale
/// peer sending the old shape must fail the handshake rather than
/// misdecode.
///
/// Version 7: one frame-vocabulary bump carrying three extensions together
/// (resolving `docs/terminal-protocol-goals.md`'s open question of whether
/// they land as one bump or two):
/// - `TerminalSpan` gains text-style attributes -- `italic`,
///   `strikethrough`, `underline` (single/double/curl/dotted/dashed), and
///   the SGR 58 `underline_color` (backlog #44).
/// - Selection becomes semantic frame metadata: `TerminalFrame::selection`
///   (viewport-space, inclusive endpoints, window-clamped) with the
///   cursor's nested-`Option` diff idiom, replacing the literal RGB
///   highlight previously baked into selected spans' `fg`/`bg` (goal 2).
/// - `TerminalCursor` gains its DECSCUSR `shape`
///   (block/underline/beam/hollow-block); a DECTCEM-hidden cursor is now
///   `cursor: None` on the wire instead of a stale always-visible block.
///
/// Version 8: `TerminalCommand` gains `SetColorScheme`, re-pushing the
/// host's live theme-derived color scheme into an already-running
/// session (a live `Reload Config`/theme-settings apply) so OSC 10/11/12
/// query replies stop reflecting a stale spawn-time snapshot. A new
/// command variant on an already-versioned vocabulary, same bump
/// discipline as every other wire-shape addition here.
///
/// Version 9: `TerminalFrame.text` removed -- it was fully derivable from
/// `lines`, and its only production reader was the `HORIZON_GPUI_DUMP`
/// debug dump (copy goes through the daemon's `selected_text`, paint never
/// read it). Dropping it removes a per-snapshot and per-diff-apply String
/// rebuild plus its share of every snapshot's wire weight; the derivation
/// survives as the debug/test helper `TerminalFrame::text()`. Removing a
/// field changes the wire shape, so a stale peer must fail the handshake.
///
/// Version 10: **the remoc cutover** (`docs/remoc-adoption-design.md`
/// §§2–3, 6). The wire is no longer JSONL envelopes at all: v10's shape is
/// the [`SessionHub`] rtc trait plus Postbag-encoded vocabularies over one
/// remoc connection, with [`SessionHub::hello`] as the first call. The
/// wire-enum catch-alls also change encoding with the codec: the JSONL
/// era's trailing `#[serde(untagged)] Unknown(UnknownPayload)` variants
/// relied on serde's `deserialize_any` buffering, which Postbag rejects
/// outright (`DeserializeAnyUnsupported` — even *known* variants stop
/// decoding), so every wire enum now carries the spike-validated
/// `#[serde(other)] Unknown` unit variant instead. A v10 peer cannot talk
/// to a v≤9 JSONL peer at all; that transition is detected by a bounded
/// connect timeout and recovered by the [`super::legacy`] drain prober, not
/// negotiated. From here on the version bumps only on a deliberate
/// semantic break: additive evolution (new `#[serde(default)]` fields,
/// new `Unknown`-guarded variants, new hub methods) ships with no version
/// event, and [`SessionHub::hello`]'s `[min_supported, current]` range
/// negotiation gates *behavior*, not decodability.
///
/// Version 11: **the frame path becomes a snapshot-valued signal**
/// (`docs/remoc-adoption-design.md` §5 Option A, ratified 2026-07-20). The
/// terminal attachment's single `updates` mpsc channel splits into a
/// `frames: rch::watch<TerminalFrame>` (every delivery a full frame; a slow
/// reader skips to the latest) and an `events: rch::mpsc<TerminalUpdate>`
/// (the non-frame updates). The wire diff machinery is deleted wholesale:
/// `TerminalFrameDiff`/`TerminalRowDiff`, `compute_frame_diff`/
/// `apply_frame_diff`, the daemon's per-connection baseline, and the
/// `TerminalUpdate::Snapshot`/`FrameDiff` variants all go — row-change
/// detection (the ShapedLine cache's invalidation signal) moves to the
/// client as a `TerminalLine` comparison of consecutive frames. A breaking
/// reshape of the terminal channel vocabulary, hence the bump; the schema
/// artifact carries it as `x-session-protocol-version`.
///
/// Version 12: **scrollback windowed overscan is negotiable**
/// (`docs/terminal-scrollback-design.md` §4, §7 phase 4). The wire surface
/// itself is *additive* and landed in v11 without a bump — the
/// `RequestScrollWindow`/`ScrollWindow` enum variants (both before their
/// `#[serde(other)] Unknown`), the `TerminalScrollWindow` payload, and the
/// `scrollback_available` `#[serde(default)]` frame flag all decode cleanly
/// on a v11 peer. This bump carries **no type change**; it is purely a
/// *feature-negotiation signal* (§3 "gates behavior, not decodability"). The
/// client sent `RequestScrollWindow` and scrolled within the served window
/// locally only when the negotiated version was ≥ 12, falling back to the
/// round-trip `Scroll` command below it. That gate constant
/// (`SCROLLBACK_WINDOW_MIN_VERSION`) and its fallback are gone since the
/// v17/v18 lockstep floor made them unreachable
/// (`docs/runtime-granularity-design.md` Q4). Because the surface was
/// additive, the minimum stayed 11 at the time: v12↔v11 negotiated 11 and
/// interoperated (tolerant evolution), rather than being rejected. The
/// schema artifact carries the bump as `x-session-protocol-version` even
/// though no wire type moved.
///
/// Version 13: **structured terminal input**
/// (`docs/terminal-kitty-associated-text-design.md` §3). `TerminalCommand`
/// gains `KeyInput`/`TextInput`, carrying the platform's associated text so
/// the daemon encodes it per the live Kitty keyboard mode instead of
/// re-deriving it. Additive (new variants before the `#[serde(other)]`
/// catch-all), so the minimum stayed 11 and the client fell back to legacy
/// `Key`/`Input` below the gate — that gate
/// (`TERMINAL_STRUCTURED_INPUT_VERSION`) is retired with v12's, same
/// reason.
///
/// Version 14: **`MessageRole::TaskNotification`**
/// (`docs/agent-async-task-design.md` decision 2). A background `task`
/// child's completion is delivered to its requester as a message, and the
/// event log must not record that system notification as words the human
/// typed — so [`crate::contract::MessageRole`] gained a third named
/// variant. The variant is *placed before* the enum's `#[serde(other)]
/// Unknown` catch-all, per this workspace's standing "keep `Unknown` last"
/// convention, which under an index-based codec (Postbag) shifts
/// `Unknown`'s index: the schema checker classifies that as a reordering
/// reshape rather than an appended value, and this bump is its required
/// version marker. Decodability is nonetheless preserved in the direction
/// that matters — an older peer decodes the new variant as `Unknown`,
/// which it already renders as assistant-authored rather than inventing
/// user words — so [`MIN_SUPPORTED_AGENT_PROTOCOL_VERSION`] stayed 11 and
/// v14↔v11 still negotiated 11 and interoperated. Nothing else in the wire
/// surface moved: the whole async-`task` mechanism (launch receipt,
/// notification queue, wake, `task_output`) is in-process inside the agent
/// daemon and crosses no channel.
/// Version 15: **`Event::HistoryCleared`**
/// (`docs/agent-compaction-design.md` Tier 1). A compaction pass freezes
/// which old tool results stop being sent to the provider verbatim, and
/// that decision has to survive a resume -- so it is a persisted, replayed
/// event rather than in-memory session state, and
/// [`crate::contract::Event`] gained a variant for it. Exactly the
/// same mechanical situation as v14: the variant is *placed before* the
/// enum's `#[serde(other)] Unknown` catch-all, per this workspace's standing
/// "keep `Unknown` last" convention, which under an index-based codec
/// (Postbag) shifts `Unknown`'s index; the schema checker classifies that as
/// a reordering reshape rather than an appended value, and this bump is its
/// required version marker. Decodability is preserved in the direction that
/// matters -- an older peer reads the new event as `Unknown` and skips it
/// (no frame item, no projection row) -- so
/// [`MIN_SUPPORTED_AGENT_PROTOCOL_VERSION`] stayed 11 and v15↔v11 still
/// negotiated 11 and interoperated. Nothing else moved: the `/models`
/// lookup, the provider-view projection, and the cleared set itself all
/// live inside the agent daemon and cross no channel.
/// Version 16: **operator-intervention audit events**
/// (`Event::ApprovalResolved`, `Event::ContinueTurnRequested`). The event
/// log is this project's primary post-hoc analysis surface (the
/// `docs/research/` reports, the 2026-07-28 session aa95e066 dogfooding
/// retrospective), and the pre-v16 log left two operator-action shapes
/// recoverable only by inference from surrounding events: a human's
/// approve/deny on a pending `ApprovalRequested` (only the order-derived
/// `ToolCallStarted`/`ToolCallFinished` carried the resolution, and only
/// by sequence position -- itself fragile under reused `call_id` or
/// sandbox-denial retries), and a human's `ContinueTurn` after a guard
/// halt (`docs/issues/002-agent-iteration-cap-halts-real-work.md` decision
/// 3 -- the resume itself left no event at all, so an analyst couldn't
/// tell a 3-Continue-turns run from a 0-Continue-turns one without reading
/// the rig session loop's code). Both gaps close by adding two new
/// variants before the enum's `#[serde(other)] Unknown` catch-all, per
/// the same "keep `Unknown` last" convention v14/v15 introduced. The
/// Postbag-index shift that follows is the bump's mechanical reason (the
/// schema checker classifies it as a reordering reshape); decodability
/// stays preserved in the same direction as v14/v15 -- an older peer
/// decodes the new events as `Unknown` and skips them, costing the audit
/// row but nothing the user-facing transcript relies on (both events are
/// audit-only: no frame item, no projection table row, so an older peer's
/// UI is byte-identical to a same-build replay that simply never got
/// them). [`MIN_SUPPORTED_AGENT_PROTOCOL_VERSION`] stayed 11 and v16↔v11
/// still negotiated 11 and interoperated. The events are emitted at the
/// daemon seam where `Command::ApproveToolCall`/`DenyToolCall`/
/// `ContinueTurn` land in `dispatch_inbound_command`
/// (`crates/horizon-agentd/src/session/approval.rs`); the actual outbound
/// `Command::ContinueTurn` it forwards is unchanged, so an old daemon
/// running against a v16 client handles it exactly as before.
///
/// Version 17: **the terminald split** (`docs/terminald-split-design.md`).
/// The single [`SessionHub`] becomes two hubs on two sockets:
/// `horizon-agentd` keeps `hello`/`list_agents`/`new_agent`/
/// `attach_agent`/`drain`, and the three terminal methods move verbatim
/// onto the new `TerminalHub` that `horizon-terminald` serves. Removing
/// methods from an rtc trait is the bluntest reshape this wire has: the
/// macro's request enum is index-encoded under Postbag, so every surviving
/// agent method shifts index and a v16 daemon cannot decode a v17 client's
/// requests at all (nor vice versa). The minimum therefore rose to 17 with
/// it -- the second time this has happened (v11's frame-path reshape was
/// the first), and for the same reason: there is no compatibility code that
/// could make the pairing work, so `hello` rejects it and the
/// auto-drain-and-respawn path (§6) recovers.
///
/// One transition wart, deliberately accepted rather than papered over: the
/// drain that recovery sends is itself index-shifted, so a *still-running
/// v16 agent daemon* decodes it as one of its own other methods and
/// keeps running. The client reports that honestly ("kept accepting
/// connections after the drain call; stop it manually") instead of looping.
/// One `kill` of the stale daemon, once, at this version boundary; every
/// later version pairing keeps the drain aligned because `drain` stays put.
///
/// Version 18: **config-only `[provider]` reload** (`docs/terminald-split-
/// design.md` phase 1). A new `reload_provider_config` method is appended to
/// [`SessionHub`], letting `Reload Config` push a model/base-URL change to a
/// running `horizon-agentd` without a respawn. This is additive in wire
/// shape (an appended request-enum variant, appended *after* `drain` so
/// `drain`'s index is stable), but the schema checker classifies a new key
/// in the artifact's `hub` object as a reshape and therefore demands a bump.
/// Rather than special-case the checker, the bump is accepted: `current` and
/// [`MIN_SUPPORTED_AGENT_PROTOCOL_VERSION`] rise together (lockstep) because
/// the checker's conservative judgment acts as the guardrail here. A v18↔v17
/// pairing has no overlapping range, so `hello` rejects it and the existing
/// auto-drain-and-respawn path (§6) recovers — no feature-gate constant is
/// introduced. `drain`'s index is unchanged (the new variant is appended
/// after it), so the respawn recovery's drain call reaches a v17 daemon
/// normally.
pub const AGENT_PROTOCOL_VERSION: u32 = 18;

/// The oldest agent-wire version this build is still willing to negotiate
/// down to in [`SessionHub::hello`] — the low end of the advertised
/// `[min_supported, current]` range. Rises when a version carries a
/// breaking wire reshape that leaves no compatibility code behind
/// (`docs/remoc-adoption-design.md` §3).
///
/// The full v11–v18 rationale for each floor movement rides on
/// [`AGENT_PROTOCOL_VERSION`]'s history above; the short form is that v11
/// (the frame-path reshape) and v17 (the terminald split) each left no
/// compatibility code behind and raised it, the additive versions
/// (v12–v16) did not, and v18 raised it in lockstep because the schema
/// checker's conservative reshape verdict on the new `hub` key is itself
/// the guardrail. Since v18 this is the standing policy: **lockstep, no
/// per-feature gates** (owner, 2026-07-30 — same-machine self-spawned
/// daemons need honest restart, not cross-version interop), which is why
/// this constant and [`AGENT_PROTOCOL_VERSION`] are equal and why the
/// per-feature gate constants that v12/v13 introduced were deleted with
/// the phase-2 split.
pub const MIN_SUPPORTED_AGENT_PROTOCOL_VERSION: u32 = 18;

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
    /// `Reload Agent Runtime` for a config-only model/base-URL change
    /// (see `docs/terminald-split-design.md`'s config-only reload). A
    /// running session keeps its spawn-time config for its whole
    /// lifetime, so the new provider takes effect for the *next* session.
    async fn reload_provider_config(&self) -> Result<(), HubError>;
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

    /// The two hubs' version pairs are independent constants but start
    /// equal, because the phase-2 split is a crate reorganization and not a
    /// wire event: a v18 shell↔daemon pairing must exchange exactly the
    /// bytes it did before the split. (The terminal side pins the mirror
    /// image of this assertion; neither crate may name the other, so the
    /// property is checked from both ends against the literal 18.)
    #[test]
    fn the_split_started_at_the_pre_split_version() {
        assert_eq!(AGENT_PROTOCOL_VERSION, 18);
        assert_eq!(MIN_SUPPORTED_AGENT_PROTOCOL_VERSION, 18);
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
            "unknown variant `__bogus`, expected one of `Hello`, `ListAgents`, `NewAgent`, \
             `AttachAgent`, `Drain`, `ReloadProviderConfig` at line 1 column 10",
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
            probe("AttachAgent").starts_with("missing field `session_id`"),
            "{}",
            probe("AttachAgent")
        );
    }
}
