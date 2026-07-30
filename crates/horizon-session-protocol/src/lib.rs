//! The session-daemon wire, v10 onwards: `#[rtc::remote]` hub traits over a
//! remoc connection, replacing the JSONL envelope protocol this crate used
//! to own (`Envelope`, `kind` dispatch, `request_id` correlation, line
//! framing — all deleted with the cutover; see
//! `docs/remoc-adoption-design.md` §2's mapping table). The agent and
//! terminal vocabularies remain sister crates that never reference each
//! other; this crate is the one place that names both — the "thin shared
//! layer" `docs/session-daemon-design.md` decision 3 allows — and the
//! dependency direction is inverted accordingly (this crate depends on the
//! vocabulary crates, never the reverse).
//!
//! Since v17 there are **two** hubs, one per daemon
//! (`docs/terminald-split-design.md`): [`SessionHub`], served by
//! `horizon-agentd` on its socket, carries the agent domain only, and
//! [`TerminalHub`], served by `horizon-terminald` on its own sibling
//! socket, carries the terminal domain. Both share this crate's
//! negotiation vocabulary ([`ClientHello`], [`VersionRange`],
//! [`HubError`]), the codec pin, and the size caps, so a client speaks the
//! same handshake to either daemon.
//!
//! **The terminal slice is append-only from v17 on** (design decision 5):
//! `horizon-terminald` is deliberately rarely restarted — a running one
//! keeps its PTYs across every `Reload Agent Runtime` — so a reshape of
//! [`TerminalHub`], [`TerminalAttachment`], or the `horizon-terminal-core`
//! vocabularies is a *heavy* change that forces every terminal session to
//! die on the next `Reload Terminal Runtime`. Evolve it by appending (new
//! methods, new `#[serde(default)]` fields, new variants before the
//! `#[serde(other)] Unknown` catch-all) and retire slots as tombstones
//! rather than removing them. The agent slice keeps the ordinary freedom
//! the §4 skew discipline grants (v14/v15/v16 are all reshapes of it).
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
///
/// Version 14: **`MessageRole::TaskNotification`**
/// (`docs/agent-async-task-design.md` decision 2). A background `task`
/// child's completion is delivered to its requester as a message, and the
/// event log must not record that system notification as words the human
/// typed — so `horizon_agent::contract::MessageRole` gained a third named
/// variant. The variant is *placed before* the enum's `#[serde(other)]
/// Unknown` catch-all, per this workspace's standing "keep `Unknown` last"
/// convention, which under an index-based codec (Postbag) shifts
/// `Unknown`'s index: the schema checker classifies that as a reordering
/// reshape rather than an appended value, and this bump is its required
/// version marker. Decodability is nonetheless preserved in the direction
/// that matters — an older peer decodes the new variant as `Unknown`,
/// which it already renders as assistant-authored rather than inventing
/// user words — so [`MIN_SUPPORTED_PROTOCOL_VERSION`] stays 11 and v14↔v11
/// still negotiate 11 and interoperate. Nothing else in the wire surface
/// moved: the whole async-`task` mechanism (launch receipt, notification
/// queue, wake, `task_output`) is in-process inside `horizon-sessiond` and
/// crosses no channel.
/// Version 15: **`Event::HistoryCleared`**
/// (`docs/agent-compaction-design.md` Tier 1). A compaction pass freezes
/// which old tool results stop being sent to the provider verbatim, and
/// that decision has to survive a resume -- so it is a persisted, replayed
/// event rather than in-memory session state, and
/// `horizon_agent::contract::Event` gained a variant for it. Exactly the
/// same mechanical situation as v14: the variant is *placed before* the
/// enum's `#[serde(other)] Unknown` catch-all, per this workspace's standing
/// "keep `Unknown` last" convention, which under an index-based codec
/// (Postbag) shifts `Unknown`'s index; the schema checker classifies that as
/// a reordering reshape rather than an appended value, and this bump is its
/// required version marker. Decodability is preserved in the direction that
/// matters -- an older peer decodes the new event as `Unknown`, which it
/// already skips (no frame item, no projection row) -- so
/// [`MIN_SUPPORTED_PROTOCOL_VERSION`] stays 11 and v15↔v11 still negotiate
/// 11 and interoperate. Nothing else moved: the `/models` lookup, the
/// provider-view projection, and the cleared set itself all live inside
/// `horizon-sessiond` and cross no channel.
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
/// them). [`MIN_SUPPORTED_PROTOCOL_VERSION`] stays 11 and v16↔v11 still
/// negotiate 11 and interoperate. The events are emitted at the sessiond
/// seam where `Command::ApproveToolCall`/`DenyToolCall`/`ContinueTurn`
/// land in `dispatch_inbound_command` (`crates/horizon-sessiond/src/session/
/// approval.rs`); the actual outbound `Command::ContinueTurn` it forwards
/// is unchanged, so an old daemon running against a v16 client handles it
/// exactly as before.
///
/// Version 17: **the terminald split** (`docs/terminald-split-design.md`).
/// The single [`SessionHub`] becomes two hubs on two sockets:
/// `horizon-sessiond` keeps `hello`/`list_agents`/`new_agent`/
/// `attach_agent`/`drain`, and the three terminal methods move verbatim
/// onto the new [`TerminalHub`] that `horizon-terminald` serves. Removing
/// methods from an rtc trait is the bluntest reshape this wire has: the
/// macro's request enum is index-encoded under Postbag, so every surviving
/// agent method shifts index and a v16 daemon cannot decode a v17 client's
/// requests at all (nor vice versa). [`MIN_SUPPORTED_PROTOCOL_VERSION`]
/// therefore rises to 17 with it -- the second time this has happened (v11's
/// frame-path reshape was the first), and for the same reason: there is no
/// compatibility code that could make the pairing work, so `hello` rejects
/// it and the auto-drain-and-respawn path (§6) recovers.
///
/// One transition wart, deliberately accepted rather than papered over: the
/// drain that recovery sends is itself index-shifted, so a *still-running
/// v16 `horizon-sessiond`* decodes it as one of its own other methods and
/// keeps running. The client reports that honestly ("kept accepting
/// connections after the drain call; stop it manually") instead of looping.
/// One `kill` of the stale daemon, once, at this version boundary; every
/// later version pairing keeps the drain aligned because `drain` stays put.
pub const SESSION_PROTOCOL_VERSION: u32 = 17;

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
/// exactly the cross-version interop the owner requires. v13 (structured
/// terminal input) is also *additive*, so it likewise does **not** raise
/// this floor: a v13 peer negotiates 11 with a v11/v12 peer and falls back
/// to legacy `Key`/`Input` commands (`TERMINAL_STRUCTURED_INPUT_VERSION`'s
/// note), preserving lossless cross-version interop. v14
/// (`MessageRole::TaskNotification`) does not raise it either: the new
/// variant leaves compatibility code behind on both sides — the older peer
/// reads it as `Unknown` (assistant-authored, never user words) and the
/// newer peer's own `Unknown` arm is unchanged — so the pairing is
/// tolerantly lossy, not undecodable. See [`SESSION_PROTOCOL_VERSION`]'s
/// v14 note for why the bump was still required. v15
/// (`Event::HistoryCleared`) is the same shape of change for the same
/// reason and likewise does not raise this floor: an older peer reads the
/// new event as `Unknown` and skips it, which costs it only the transcript's
/// compaction divider -- never a message, a tool call, or a result. v17
/// (the terminald split) *does* raise it, to 17: removing three methods
/// from [`SessionHub`] shifts every surviving method's index under the
/// index-encoded request enum, which leaves no compatibility code behind on
/// either side -- exactly v11's situation. See
/// [`SESSION_PROTOCOL_VERSION`]'s v17 note.
pub const MIN_SUPPORTED_PROTOCOL_VERSION: u32 = 17;

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

/// The first negotiated version at which the client sends structured
/// terminal input commands (`TerminalCommand::KeyInput` and
/// `TerminalCommand::TextInput`) and the daemon encodes them with associated
/// text according to the live Kitty keyboard mode
/// (`docs/terminal-kitty-associated-text-design.md` §3). Below this (a v11/
/// v12 peer), the client falls back to legacy `Key` and raw UTF-8 `Input`.
pub const TERMINAL_STRUCTURED_INPUT_VERSION: u32 = 13;

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

/// `horizon-agentd`'s `hello` reply: the negotiated version plus the
/// connection-global channels (`docs/remoc-adoption-design.md` §2 — what
/// used to be connection-global envelope kinds now rides channels handed
/// over here; everything session-scoped rides the per-attachment channels
/// instead). Every channel here is agent-domain, which is why
/// [`TerminalHubHello`] carries none of them.
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

/// `horizon-terminald`'s `hello` reply. Deliberately channel-free: every
/// connection-global channel [`HubHello`] hands over belongs to the agent
/// domain (host tools, the event log's startup diagnostic), and the
/// terminal domain's only streams are per-attachment
/// ([`TerminalAttachment`]).
///
/// `binary_id` is load-bearing beyond diagnostics here — it is the terminald
/// connection's skew insurance (`docs/terminald-split-design.md` decision
/// 6). A terminald that outlives many UI rebuilds may be running an older
/// binary than the client that connects to it, and the layer *below* the
/// negotiated version (remoc/chmux framing, the Postbag codec) is not
/// covered by version negotiation at all — the tmux 3.6 lesson. The client
/// records this id at `hello` and names it if a post-`hello` decode failure
/// suggests exactly that kind of skew, so the failure surfaces as a clean
/// refusal pointing at `Reload Terminal Runtime` rather than as silent
/// misbehavior.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TerminalHubHello {
    /// The highest mutually supported version (see [`HubHello::negotiated`]).
    pub negotiated: u32,
    pub binary_id: String,
}

/// The schema stand-in for a remoc channel half: on the wire it is a chmux
/// port reference, not data, so the artifact documents it as an opaque
/// marker. What flows *through* each channel is documented separately by
/// the artifact's `channels` section (see
/// `crates/horizon-agentd/tests/wire_schema.rs`).
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

/// The terminal hub — `horizon-terminald`'s whole rtc surface
/// (`docs/terminald-split-design.md` decision 1). Carved off [`SessionHub`]
/// in v17 so terminal hosting lives in its own rarely-restarted process:
/// `Reload Agent Runtime` drains the *agent* daemon and never touches a
/// PTY, while `Reload Terminal Runtime` is the explicit, destructive
/// counterpart for this one.
///
/// **Append-only from here on** (design decision 5, owner-accepted): every
/// restart of the daemon serving this trait kills real interactive shells,
/// so a reshape is a heavy, user-visible change. New methods and new
/// `#[serde(default)]`/`Unknown`-guarded vocabulary go on the end; a retired
/// method becomes a tombstone that errors, never a hole. The version range
/// [`hello`](Self::hello) negotiates still gates *behavior* the same way it
/// does on [`SessionHub`].
#[rtc::remote]
pub trait TerminalHub {
    /// Version negotiation, identical in shape to [`SessionHub::hello`] —
    /// the first call on every connection, and (with
    /// [`drain`](Self::drain)) the version-stable surface a
    /// range-rejected client may still use.
    async fn hello(&self, client: ClientHello) -> Result<TerminalHubHello, HubError>;

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

    /// Flush-and-exit: shuts every hosted terminal down and exits. The
    /// destructive half of `Reload Terminal Runtime`; like
    /// [`SessionHub::drain`] the call itself typically errors because the
    /// process is gone before a reply can travel.
    async fn drain(&self) -> Result<(), HubError>;
}

/// The agent session hub — `horizon-agentd`'s rtc surface
/// (`docs/remoc-adoption-design.md` §2). The daemon serves it over its unix
/// socket; [`hello`](Self::hello) must be the first call on every
/// connection. `hello` and [`drain`](Self::drain) are the version-stable
/// surface (the conversations that must keep working across future protocol
/// versions, like the JSONL era's `session_control` kind); everything else
/// may evolve additively under the §4 skew discipline.
///
/// Terminal hosting left this trait in v17 — see [`TerminalHub`].
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
    /// [`TerminalHub::drain`] is the destructive counterpart.
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
            "unknown variant `__bogus`, expected one of `Hello`, `ListAgents`, `NewAgent`, \
             `AttachAgent`, `Drain` at line 1 column 10",
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

    /// [`TerminalHub`]'s half of the same mechanical check — and the guard
    /// behind the v17 split's central claim that the terminal methods moved
    /// *verbatim*: the same names, the same argument names, on the daemon
    /// that now owns them.
    #[test]
    fn terminal_hub_request_enum_matches_the_documented_method_surface() {
        let variants =
            match serde_json::from_str::<TerminalHubReqRef<WireCodec>>("{\"__bogus\":null}") {
                Ok(_) => panic!("a bogus variant must fail"),
                Err(error) => error.to_string(),
            };
        assert_eq!(
            variants,
            "unknown variant `__bogus`, expected one of `Hello`, `ListTerminals`, \
             `CreateTerminal`, `AttachTerminal`, `Drain` at line 1 column 10",
        );

        let probe = |method: &str| {
            let json = format!(
                "{{\"{method}\": {{\"__reply_tx\": {{\"port\": null, \"data\": null, \
                 \"codec\": null}}}}}}"
            );
            match serde_json::from_str::<TerminalHubReqRef<WireCodec>>(&json) {
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
        // `create_terminal`'s second argument, past the first.
        let spec_probe = "{\"CreateTerminal\": {\"__reply_tx\": {\"port\": null, \
             \"data\": null, \"codec\": null}, \"session_id\": \
             \"00000000-0000-0000-0000-000000000000\"}}";
        match serde_json::from_str::<TerminalHubReqRef<WireCodec>>(spec_probe) {
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
