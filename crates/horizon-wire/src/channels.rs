//! Per-purpose wire size caps, the capped channel aliases that carry them,
//! and the schema stand-in a transported channel half renders as.
//!
//! remoc's own default (`rch::DEFAULT_MAX_ITEM_SIZE`, 16 MiB) is one
//! blanket limit for everything; these caps size each channel class to what
//! it actually carries, so a runaway (or corrupted) item fails as a
//! per-item error instead of buffering many megabytes. Enforcement follows
//! rch's transport model: a receiver's cap is its `MAX_ITEM_SIZE` const
//! parameter (carried with the receiver when it is transported — see
//! [`CappedReceiver`]/[`CappedWatchReceiver`]), which bounds *both* the
//! forwarding serialize on the sending peer and the decode on the receiving
//! peer (the receiver is what every attachment channel here transports).
//!
//! The skip-vs-fatal boundary differs by channel kind, and this matters for
//! the frame path:
//!
//! - **`rch::mpsc`** (events, commands, tool I/O): an item that is oversized
//!   or undecodable is a *per-item* `recv` error the loops skip (adoption
//!   condition 2), and the channel survives.
//! - **`rch::watch`** (the v11 frame path): the *receive* side is equally
//!   forgiving — a `Deserialize`/`MaxItemSizeExceeded` value is
//!   non-final (`is_final() == false`), published as an error value the
//!   reader skips, and the channel self-heals on the next frame. But the
//!   *send* (forwarding) side is not: an item the daemon fails to serialize
//!   within the cap is item-specific and breaks that watch, closing it. That
//!   is acceptable because the frame path's resync is structural — the client
//!   handles a frame-watch close by re-attaching (a fresh watch reseeded with
//!   the retained latest frame), and [`FRAME_MAX_ITEM_BYTES`] carries an
//!   order of magnitude of headroom over real frames, so this is
//!   pathological, not a routine skip.

use remoc::prelude::*;
use schemars::JsonSchema;

use crate::codec::WireCodec;

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

/// The schema stand-in for a remoc channel half: on the wire it is a chmux
/// port reference, not data, so the artifact documents it as an opaque
/// marker. What flows *through* each channel is documented separately by
/// the artifact's `channels` section -- one artifact per runtime crate,
/// generated beside the hub that owns it
/// (`crates/horizon-agent/tests/wire_schema.rs` and
/// `crates/horizon-terminal-core/tests/wire_schema.rs`).
pub fn channel_schema<T: JsonSchema>(
    generator: &mut schemars::SchemaGenerator,
) -> schemars::Schema {
    let payload = generator.subschema_for::<T>();
    schemars::json_schema!({
        "$comment": "remoc rch channel half: a chmux port reference on the wire; the \
                     x-channel-payload schema is what flows through it",
        "x-channel-payload": payload,
    })
}
