//! The session-daemon wire, v10 onwards: one `#[rtc::remote]` hub trait
//! ([`SessionHub`]) over a remoc connection, replacing the JSONL envelope
//! protocol this crate used to own (`Envelope`, `kind` dispatch,
//! `request_id` correlation, line framing — all deleted with the cutover;
//! see `docs/remoc-adoption-design.md` §2's mapping table). The agent and
//! terminal vocabularies remain sister crates that never reference each
//! other; this crate is the one place that names both — the "thin shared
//! layer" `docs/session-daemon-design.md` decision 3 allows — and the
//! dependency direction is inverted accordingly (this crate depends on the
//! vocabulary crates, never the reverse).
//!
//! Adoption conditions (binding, §1 of the design doc), as implemented
//! here:
//!
//! 1. **The codec is pinned, never defaulted**: every channel field and
//!    every server/client construction names [`WireCodec`] (Postbag, Full
//!    configuration); the workspace disables remoc's `default-codec-*`
//!    features so `codec::Default` fails to compile if anything names it.
//!    Postbag is not self-describing, so the vocabularies' free-form JSON
//!    payloads (tool inputs/outputs) travel as
//!    `horizon_agent::contract::JsonValue` — their JSON text in one
//!    string — rather than `serde_json::Value`, whose `Deserialize`
//!    needs `deserialize_any`.
//! 2. **Every wire enum carries a `#[serde(other)] Unknown` catch-all**,
//!    and receive loops treat a non-final deserialization error as "skip
//!    this item", never "tear down the channel".
//! 3. **`Connect::io` is polled on both ends concurrently** — in-process
//!    harnesses hosting both endpoints must `join!` the two handshakes
//!    (sequentially awaiting one side deadlocks and presents as a 60 s
//!    `ChMux(Timeout)`).

use horizon_agent::contract::{Command, SessionId};
use horizon_agent::wire::{
    AgentWireEvent, HostToolRequest, HostToolResponse, SessionNew, SessionSummary,
};
use horizon_terminal_core::{
    TerminalCommand, TerminalFrame, TerminalSpawnSpec, TerminalSummary, TerminalUpdate,
};
use remoc::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod legacy;
pub mod schema_check;

/// The one wire codec, named everywhere a codec parameter appears
/// (adoption condition 1): Postbag in its Full configuration — the exact
/// configuration the spike's perf/skew experiments validated
/// (`docs/research/remoc-spike-2026-07-20.md` §§1–2). Never
/// `remoc::codec::Default`: the workspace builds remoc with default
/// features off, so the default-codec alias deliberately does not resolve
/// to a usable codec and a remoc upgrade that changes its default cannot
/// silently fork the wire.
///
/// Postbag is **not self-describing** (`deserialize_any` is rejected), so
/// the vocabularies' free-form JSON payloads — tool call inputs/outputs —
/// cross this wire as `horizon_agent::contract::JsonValue` (their JSON
/// text in one string) instead of `serde_json::Value`; see that type's
/// doc for the format-aware encoding that keeps the event log's on-disk
/// JSONL format byte-identical.
pub type WireCodec = remoc::codec::Postbag;

// ---------------------------------------------------------------------------
// Per-purpose wire size caps.
//
// remoc's own default (`rch::DEFAULT_MAX_ITEM_SIZE`, 16 MiB) is one
// blanket limit for everything; these caps size each channel class to what
// it actually carries, so a runaway (or corrupted) item fails as a
// per-item error instead of buffering many megabytes. Enforcement follows
// rch's transport model: a receiver's cap is its `MAX_ITEM_SIZE` const
// parameter (carried with the receiver when it is transported — see
// [`CappedReceiver`]/[`CappedWatchReceiver`]), which bounds *both* the
// forwarding serialize on the sending peer and the decode on the receiving
// peer (the receiver is what every attachment channel here transports).
//
// The skip-vs-fatal boundary differs by channel kind, and this matters for
// the frame path:
//
// - **`rch::mpsc`** (events, commands, tool I/O): an item that is oversized
//   or undecodable is a *per-item* `recv` error the loops skip (adoption
//   condition 2), and the channel survives.
// - **`rch::watch`** (the v11 frame path): the *receive* side is equally
//   forgiving — a `Deserialize`/`MaxItemSizeExceeded` value is
//   non-final (`is_final() == false`), published as an error value the
//   reader skips, and the channel self-heals on the next frame. But the
//   *send* (forwarding) side is not: an item the daemon fails to serialize
//   within the cap is item-specific and breaks that watch, closing it. That
//   is acceptable because the frame path's resync is structural — the client
//   handles a frame-watch close by re-attaching (a fresh watch reseeded with
//   the retained latest frame), and [`FRAME_MAX_ITEM_BYTES`] carries an
//   order of magnitude of headroom over real frames, so this is
//   pathological, not a routine skip.
// ---------------------------------------------------------------------------

/// Terminal frame channel items (`TerminalFrame`). Since wire v11 the
/// frame path is an `rch::watch<TerminalFrame>` snapshot-valued signal
/// (`docs/remoc-adoption-design.md` §5 Option A), so this caps every full
/// frame the daemon publishes (the watch keeps only the latest; slow
/// readers converge). A full 200x50 fully-styled snapshot measured ~50 KB
/// under Postbag (`docs/research/remoc-spike-2026-07-20.md` §1a: 178,743 B
/// under the *worst-case* all-rows-styled synthetic frame); 4 MiB leaves an
/// order of magnitude of headroom for pathological scrollback/styling
/// without admitting absurd buffers. The cap is the watch *receiver's*
/// `MAX_ITEM_SIZE` const parameter (carried with the receiver when
/// transported): the attachment transports the receiver, so this one const
/// bounds both the daemon's forwarding serialize (`send_impl`) and the
/// client's decode. The watch *sender's* runtime `max_item_size` is inert
/// in this topology — nothing transports the sender — so it is not set. See
/// [`CappedWatchReceiver`].
pub const FRAME_MAX_ITEM_BYTES: usize = 4 * 1024 * 1024;

/// Terminal *event* channel items (`TerminalUpdate`: title, bell, clipboard,
/// exit, error — everything the frame watch does not carry). Sized to match
/// the frame cap (4 MiB) so it regresses the v10 effective limit: v10's
/// `Clipboard` rode the single 4 MiB `updates` mpsc, and an OSC 52 copy of a
/// full-screen selection is user-scaled — capping events at a tighter 1 MiB
/// would silently shrink that ceiling to a quarter.
pub const TERMINAL_EVENT_MAX_ITEM_BYTES: usize = 4 * 1024 * 1024;

/// Command-channel items (`TerminalCommand`, agent `Command`) and the
/// JSON-payload-bearing tool exchanges (`AgentWireEvent`,
/// `HostToolRequest`/`HostToolResponse`). Sized at 1 MiB rather than a
/// tighter control-plane cap because these legitimately carry user-scaled
/// data: a terminal `Paste`/`Input` is whatever the user pasted, and tool
/// inputs/outputs (`JsonValue`) carry file contents.
pub const COMMAND_MAX_ITEM_BYTES: usize = 1024 * 1024;

/// See [`COMMAND_MAX_ITEM_BYTES`] — the tool-I/O alias, kept separate so
/// the two classes can diverge without a wire-wide sweep.
pub const TOOL_IO_MAX_ITEM_BYTES: usize = 1024 * 1024;

/// Small control-plane strings (the `skipped_lines` startup diagnostic).
pub const CONTROL_MAX_ITEM_BYTES: usize = 64 * 1024;

/// One rtc request (`hello`, `create_terminal(spec)`, `new_agent(new)`,
/// ...). Requests are small structured arguments — specs, ids, version
/// ranges — never bulk data, so exceeding this is always a bug, and the
/// consequence is deliberately blunt: the daemon drops the oversized
/// request per-item, the call fails when its reply channel closes, and —
/// because rch latches the remote-send error onto the transported
/// request channel — the connection then tears down (measured; pinned by
/// `an_oversized_rtc_request_fails_the_op_and_stops_the_runtime`). Bulk
/// data has its own channels with their own caps.
pub const RTC_MAX_REQUEST_BYTES: usize = 64 * 1024;

/// One rtc reply (`HubHello`, attachments, `list_*` vectors). Sized to hold
/// a terminal attachment, which is the largest reply: since v11 the
/// attachment's frames watch **inlines its seed (the retained latest frame)
/// into the reply** — `rch::watch::Receiver`'s serializer snapshots
/// `borrow()` — so a re-attach to a session holding a large frame must not
/// be rejected for a frame the *live* watch (a [`FRAME_MAX_ITEM_BYTES`]
/// port) already accepts. Hence one full frame plus 1 MiB of envelope /
/// `list_*` headroom (those scale with live session count). Without this,
/// attaching to a session whose retained frame exceeds 1 MiB failed
/// permanently while live delivery of the same frame succeeded.
pub const RTC_MAX_REPLY_BYTES: usize = FRAME_MAX_ITEM_BYTES + 1024 * 1024;

/// A receiver whose per-item size cap is part of its type: rch enforces
/// the receive-direction cap through the receiver's `MAX_ITEM_SIZE`
/// const parameter (carried with the receiver when it is transported), so
/// a capped channel field must *name* its cap — `channel()` +
/// [`remoc::rch::mpsc::Receiver::set_max_item_size`] produce one. The
/// send-direction caps, by contrast, are runtime state on the sender set
/// by its creator *before* handing it over (a transported sender carries
/// the creator's cap as the creator-side receive limit).
pub type CappedReceiver<T, const MAX_ITEM_SIZE: usize> =
    rch::mpsc::Receiver<T, WireCodec, { rch::DEFAULT_BUFFER }, MAX_ITEM_SIZE>;

/// The `rch::watch` counterpart of [`CappedReceiver`] — a watch receiver
/// whose per-item size cap is its `MAX_ITEM_SIZE` const parameter (carried
/// with it when transported, exactly as for mpsc). The frame path
/// (`docs/remoc-adoption-design.md` §5 Option A) is the one channel class
/// that uses it: a snapshot-valued signal where the current value is always
/// the latest full frame, a slow reader observes a skipping sequence that
/// converges on the final value, and there is no queue to bound — only the
/// per-frame item size, which [`FRAME_MAX_ITEM_BYTES`] sets. The daemon
/// transports this receiver and keeps the sender, so the receiver's const
/// is the effective cap in *both* directions: `rch::watch::Receiver`'s
/// serializer drives the forwarding with this const, and the client's
/// deserializer decodes with it. The sender's runtime `max_item_size` is
/// therefore never consulted in this topology and is left unset (see
/// [`FRAME_MAX_ITEM_BYTES`]); set only this receiver const with
/// [`set_max_item_size`](remoc::rch::watch::Receiver::set_max_item_size).
pub type CappedWatchReceiver<T, const MAX_ITEM_SIZE: usize> =
    rch::watch::Receiver<T, WireCodec, MAX_ITEM_SIZE>;

/// A rate-limited log for the receive/send loops' skip paths (adoption
/// condition 2: a poisoned item is skipped, never fatal): logs the first
/// occurrence, then only at powers of two and every 1000th, with the
/// running count, so a peer stuck emitting undecodable items cannot
/// flood stderr at channel throughput.
pub struct DecodeSkipLog {
    label: &'static str,
    skipped: u64,
}

impl DecodeSkipLog {
    pub const fn new(label: &'static str) -> Self {
        Self { label, skipped: 0 }
    }

    /// Records one skipped item, logging at 1, 10, 100, 1000, ...
    pub fn note(&mut self, error: &dyn std::fmt::Display) {
        self.skipped += 1;
        if self.skipped.is_power_of_two() || self.skipped.is_multiple_of(1000) {
            eprintln!(
                "{}: skipping an undecodable item (#{} so far): {error}",
                self.label, self.skipped
            );
        }
    }

    pub fn skipped(&self) -> u64 {
        self.skipped
    }
}

/// The session-daemon protocol version this build speaks.
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
/// connect timeout and recovered by the [`legacy`] drain prober, not
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
/// client sends `RequestScrollWindow` and scrolls within the served window
/// locally only when the negotiated version is ≥ 12; a v12 client that
/// negotiates 11 against an older daemon falls back to today's round-trip
/// `Scroll` command, so a v11 daemon that never serves a window can't leave
/// the client waiting on one. Because the surface is additive,
/// [`MIN_SUPPORTED_PROTOCOL_VERSION`] stays 11: v12↔v11 negotiate 11 and
/// interoperate (tolerant evolution), rather than being rejected. The schema
/// artifact carries the bump as `x-session-protocol-version` even though no
/// wire type moved.
pub const SESSION_PROTOCOL_VERSION: u32 = 12;

/// The oldest protocol version this build is still willing to negotiate
/// down to in [`SessionHub::hello`] — the low end of the advertised
/// `[min_supported, current]` range. Rises when a version carries a
/// breaking wire reshape that leaves no compatibility code behind
/// (`docs/remoc-adoption-design.md` §3). v11's frame-path reshape (§5
/// Option A) is exactly that: the v11 `TerminalAttachment` shape (a
/// `watch<TerminalFrame>` + an events mpsc) is structurally undecodable to
/// a v10 peer and vice-versa, so this build cannot honor a negotiated v10.
/// A v10↔v11 pairing therefore has no overlapping range and `hello` rejects
/// it — recovered by the auto-drain-and-respawn path (§6), not negotiated.
/// v12 (scrollback windowing) is *additive*, not a reshape, so it does **not**
/// raise this floor: a v12 peer negotiates 11 with a v11 peer and falls back
/// to round-trip scrolling (`SESSION_PROTOCOL_VERSION`'s v12 note), which is
/// exactly the cross-version interop the owner requires.
pub const MIN_SUPPORTED_PROTOCOL_VERSION: u32 = 11;

/// The first negotiated version at which the daemon answers
/// `TerminalCommand::RequestScrollWindow` with a served window
/// (`docs/terminal-scrollback-design.md` §4). The client sends window
/// requests — and scrolls locally within the reply — only when the
/// connection's negotiated version is at least this; below it (a v11 daemon
/// that never serves a window) it falls back to the round-trip `Scroll`
/// command. Deliberately a distinct constant from
/// [`SESSION_PROTOCOL_VERSION`] so a later, unrelated version bump cannot
/// silently move the feature gate.
pub const SCROLLBACK_WINDOW_MIN_VERSION: u32 = 12;

/// An inclusive protocol-version range one peer supports, as exchanged in
/// [`SessionHub::hello`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VersionRange {
    pub min_supported: u32,
    pub current: u32,
}

impl VersionRange {
    /// The range this build advertises.
    pub fn ours() -> Self {
        Self {
            min_supported: MIN_SUPPORTED_PROTOCOL_VERSION,
            current: SESSION_PROTOCOL_VERSION,
        }
    }

    /// The highest version both ranges support, if the ranges overlap.
    pub fn negotiate(self, other: Self) -> Option<u32> {
        let low = self.min_supported.max(other.min_supported);
        let high = self.current.min(other.current);
        (low <= high).then_some(high)
    }
}

impl std::fmt::Display for VersionRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[v{}, v{}]", self.min_supported, self.current)
    }
}

/// The client half of the version negotiation, carried by the first rtc
/// call on every connection ([`SessionHub::hello`]).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClientHello {
    pub supported: VersionRange,
    pub binary_id: String,
}

impl ClientHello {
    pub fn new(binary_id: impl Into<String>) -> Self {
        Self {
            supported: VersionRange::ours(),
            binary_id: binary_id.into(),
        }
    }
}

/// The daemon's `hello` reply: the negotiated version plus the
/// connection-global channels (`docs/remoc-adoption-design.md` §2 — what
/// used to be connection-global envelope kinds now rides channels handed
/// over here; everything session-scoped rides the per-attachment channels
/// instead).
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

/// The schema stand-in for a remoc channel half: on the wire it is a chmux
/// port reference, not data, so the artifact documents it as an opaque
/// marker. What flows *through* each channel is documented separately by
/// the artifact's `channels` section (see
/// `crates/horizon-sessiond/tests/wire_schema.rs`).
fn channel_schema<T: JsonSchema>(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let payload = generator.subschema_for::<T>();
    schemars::json_schema!({
        "$comment": "remoc rch channel half: a chmux port reference on the wire; the \
                     x-channel-payload schema is what flows through it",
        "x-channel-payload": payload,
    })
}

/// What [`SessionHub::create_terminal`]/[`SessionHub::attach_terminal`]
/// hand back: the session's live channels. Since wire v11
/// (`docs/remoc-adoption-design.md` §5 Option A) frame delivery is a
/// snapshot-valued signal — `frames` is an `rch::watch<TerminalFrame>`
/// whose current value *is* the latest frame at every moment, seeded on
/// attach with the daemon-retained latest frame. The wire diff machinery is
/// gone: no `Snapshot`/`FrameDiff` split, no per-connection baseline, and
/// row-change detection moved to the client (a `TerminalLine` comparison of
/// consecutive frames). `events` carries everything that is *not* a frame
/// (`TerminalUpdate`: title, bell, clipboard, exit, error).
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TerminalAttachment {
    /// The snapshot-valued frame signal: every observation is a full
    /// [`TerminalFrame`]; a slow reader skips intermediate frames and
    /// converges on the latest (§5 Option A, spike §1c). Seeded with the
    /// daemon-retained latest frame on attach (the structural resync
    /// anchor), or an empty frame for a freshly created session.
    #[schemars(schema_with = "channel_schema::<TerminalFrame>")]
    pub frames: CappedWatchReceiver<TerminalFrame, FRAME_MAX_ITEM_BYTES>,
    #[schemars(schema_with = "channel_schema::<TerminalUpdate>")]
    pub events: CappedReceiver<TerminalUpdate, TERMINAL_EVENT_MAX_ITEM_BYTES>,
    #[schemars(schema_with = "channel_schema::<TerminalCommand>")]
    pub commands: rch::mpsc::Sender<TerminalCommand, WireCodec>,
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

/// The hub's error vocabulary. One enum for every method: domain errors
/// and transport errors share it, per remoc's own rtc pattern (the
/// `From<rtc::CallError>` impl is what lets a lost connection surface as
/// an `Err` from any pending call).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
pub enum HubError {
    /// `hello`: the peers' version ranges do not overlap. Feeds the same
    /// auto-drain recovery as the JSONL era's `HandshakeRejected`.
    #[error("session protocol version ranges do not overlap: client {client}, daemon {daemon}")]
    IncompatibleVersion {
        client: VersionRange,
        daemon: VersionRange,
    },
    /// `attach_terminal`: no live terminal session with that id.
    #[error("no live terminal session with that id")]
    TerminalNotFound,
    /// `create_terminal`: the PTY spawn itself failed (bad shell,
    /// permissions, or the bounded spawn retries were exhausted). What the
    /// JSONL wire reported as a `TerminalUpdate::Error` on the update
    /// stream is now the create call's own result.
    #[error("terminal failed to start: {0}")]
    TerminalSpawnFailed(String),
    /// Transport failure of the rtc call itself, carried as its rendered
    /// message (`rtc::CallError` itself is not `Eq`/`JsonSchema`; nothing
    /// programmatic branches on its inner structure). Constructed
    /// client-side by the `From<rtc::CallError>` impl below — a server
    /// never sends it.
    #[error("hub call failed: {0}")]
    Call(String),
    /// Any method other than `hello`/`drain` was called before a
    /// successful `hello` on this connection. `hello` is contractually the
    /// first call (§3), and the daemon enforces it rather than trusting
    /// the client: a rejected or skipped negotiation must not grant access
    /// to the negotiated-behavior surface. (`drain` stays reachable — it
    /// is the version-stable recovery path a rejected client legitimately
    /// uses.) Appended additively for v10.1 of the artifact's history —
    /// an older client never triggers it (it always hellos first).
    #[error("hello has not completed on this connection")]
    HelloRequired,
    /// Skew catch-all: an error variant from a newer peer. Keep last.
    #[serde(other)]
    #[error("unknown hub error from a newer peer")]
    Unknown,
}

impl From<rtc::CallError> for HubError {
    fn from(err: rtc::CallError) -> Self {
        Self::Call(err.to_string())
    }
}

/// The session hub — the one `#[rtc::remote]` trait that replaces the
/// envelope protocol (`docs/remoc-adoption-design.md` §2). The daemon
/// serves it over the unix socket; [`hello`](Self::hello) must be the
/// first call on every connection. `hello` and
/// [`drain`](Self::drain) are the version-stable surface (the
/// conversations that must keep working across future protocol versions,
/// like the JSONL era's `session_control` kind); everything else may
/// evolve additively under the §4 skew discipline.
#[rtc::remote]
pub trait SessionHub {
    /// Version negotiation (`docs/remoc-adoption-design.md` §3): the first
    /// call on every connection. Replaces the exact-match JSONL handshake
    /// with `[min_supported, current]` range intersection.
    async fn hello(&self, client: ClientHello) -> Result<HubHello, HubError>;

    // -- terminal domain --

    /// Every live terminal session, sorted by id. Replaces the
    /// request-id-correlated `TerminalControl::List`/`ListResult` pair.
    async fn list_terminals(&self) -> Result<Vec<TerminalSummary>, HubError>;

    /// Spawns a PTY for `session_id` and attaches to it. The id is
    /// caller-chosen (the workspace model owns pane identity), exactly as
    /// the JSONL `Create` control's envelope `session_id` was.
    async fn create_terminal(
        &self,
        session_id: Uuid,
        spec: TerminalSpawnSpec,
    ) -> Result<TerminalAttachment, HubError>;

    /// Attaches to an already-running terminal session. The returned
    /// attachment's `frames` watch is seeded with the daemon-retained latest
    /// frame — the structural resync anchor: since v11 the watch's current
    /// value *is* the latest frame, so there is no baseline to establish and
    /// no snapshot-then-diffs dance.
    async fn attach_terminal(&self, session_id: Uuid) -> Result<TerminalAttachment, HubError>;

    // -- agent domain --

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
    /// flushes its event log to disk, shuts its terminals down, and
    /// exits. The call itself typically errors (the process is gone
    /// before a reply can travel); callers observe completion as the
    /// socket refusing connections, same as before.
    async fn drain(&self) -> Result<(), HubError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ranges_negotiate_to_the_highest_shared_version() {
        let ours = VersionRange {
            min_supported: 10,
            current: 12,
        };
        let theirs = VersionRange {
            min_supported: 11,
            current: 14,
        };
        assert_eq!(ours.negotiate(theirs), Some(12));
        assert_eq!(theirs.negotiate(ours), Some(12));
    }

    #[test]
    fn disjoint_version_ranges_do_not_negotiate() {
        let ours = VersionRange {
            min_supported: 10,
            current: 10,
        };
        let theirs = VersionRange {
            min_supported: 11,
            current: 14,
        };
        assert_eq!(ours.negotiate(theirs), None);
        assert_eq!(theirs.negotiate(ours), None);
    }

    #[test]
    fn our_range_negotiates_with_itself_at_the_current_version() {
        assert_eq!(
            VersionRange::ours().negotiate(VersionRange::ours()),
            Some(SESSION_PROTOCOL_VERSION)
        );
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
            "unknown variant `__bogus`, expected one of `Hello`, `ListTerminals`, \
             `CreateTerminal`, `AttachTerminal`, `ListAgents`, `NewAgent`, `AttachAgent`, \
             `Drain` at line 1 column 10",
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
            probe("CreateTerminal").starts_with("missing field `session_id`"),
            "{}",
            probe("CreateTerminal")
        );
        assert!(
            probe("AttachTerminal").starts_with("missing field `session_id`"),
            "{}",
            probe("AttachTerminal")
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
        // `create_terminal`'s second argument, past the first.
        let spec_probe = "{\"CreateTerminal\": {\"__reply_tx\": {\"port\": null, \
             \"data\": null, \"codec\": null}, \"session_id\": \
             \"00000000-0000-0000-0000-000000000000\"}}";
        match serde_json::from_str::<SessionHubReqRef<WireCodec>>(spec_probe) {
            Ok(_) => panic!("spec must still be required"),
            Err(error) => assert!(
                error.to_string().starts_with("missing field `spec`"),
                "{error}"
            ),
        }
    }

    /// An unknown `HubError` variant from a newer peer degrades to
    /// `Unknown` under the wire codec (Postbag), instead of failing the
    /// reply — the §4 catch-all, proven on the one enum this crate owns.
    #[test]
    fn unknown_hub_error_variant_degrades_to_unknown_under_postbag() {
        #[derive(Serialize)]
        enum FutureHubError {
            SomethingNew { detail: String },
        }
        let mut bytes = Vec::new();
        <WireCodec as remoc::codec::Codec>::serialize(
            &mut bytes,
            &FutureHubError::SomethingNew {
                detail: "later".into(),
            },
        )
        .unwrap();
        let decoded: HubError =
            <WireCodec as remoc::codec::Codec>::deserialize(&bytes[..]).unwrap();
        assert_eq!(decoded, HubError::Unknown);
    }
}
