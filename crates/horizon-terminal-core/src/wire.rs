//! The terminal runtime's hub — `horizon-terminald`'s whole rtc surface —
//! and the version pair it negotiates.
//!
//! Until `docs/runtime-crate-alignment-design.md` phase 2 this trait lived
//! in `horizon-session-protocol` next to the agent hub, sharing one
//! `SESSION_PROTOCOL_VERSION`. Under the lockstep policy that shared
//! constant is what made an agent-only wire addition (v18 was exactly one)
//! reject the running `horizon-terminald` too and auto-drain it — every PTY
//! dying for a change that never touched the terminal wire. Judgment 3
//! dissolved the union: this hub now lives in the runtime crate that owns
//! it, with its own version pair
//! ([`TERMINAL_PROTOCOL_VERSION`]/[`MIN_SUPPORTED_TERMINAL_PROTOCOL_VERSION`])
//! and its own schema artifact (`schema/terminal-wire.json`), while the
//! domain-free foundation stays in `horizon-wire` ([`ClientHello`],
//! [`VersionRange`], the codec pin, the size caps, and [`HubError`]).
//!
//! remoc appears in this crate for this module only: the emulation engine,
//! the session loop, and the vocabulary types stay serde-plain and
//! transport-free, exactly as `docs/remoc-adoption-design.md` §1's
//! exit-cost note requires.

use remoc::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use horizon_wire::{
    channel_schema, CappedReceiver, CappedWatchReceiver, ClientHello, HubError, VersionRange,
    WireCodec, FRAME_MAX_ITEM_BYTES, TERMINAL_EVENT_MAX_ITEM_BYTES,
};

use crate::{TerminalCommand, TerminalFrame, TerminalSpawnSpec, TerminalSummary, TerminalUpdate};

/// The terminal-daemon protocol version this build speaks.
///
/// The v4–v18 history of the single pre-split constant — including the
/// terminal-side bumps (v5's owned colors, v7's frame styles/selection/
/// cursor shape, v8's `SetColorScheme`, v9's dropped `TerminalFrame.text`,
/// v11's snapshot-valued frame path, v12's scrollback windowing, v13's
/// structured input, v17's carve-out of this very trait) — is recorded once,
/// whole, on `horizon_agent::wire::AGENT_PROTOCOL_VERSION`. It is not
/// duplicated or divided here: several of those bumps were terminal changes
/// that moved the *shared* number, so splitting the narrative by domain
/// would rewrite what happened. From 18 on the two constants move
/// independently, and only genuinely terminal-side history will be recorded
/// here.
///
/// **This slice is append-only** (`docs/terminald-split-design.md` decision
/// 5, owner-accepted): `horizon-terminald` is deliberately rarely restarted
/// — a running one keeps its PTYs across every `Reload Agent Runtime` — so a
/// reshape of [`TerminalHub`], [`TerminalAttachment`], or this crate's
/// vocabularies is a *heavy* change that kills every live shell on the next
/// `Reload Terminal Runtime`. Evolve by appending (new methods, new
/// `#[serde(default)]` fields, new variants before the `#[serde(other)]
/// Unknown` catch-all) and retire slots as tombstones rather than removing
/// them. Splitting the version pair is what finally makes that discipline
/// pay: an agent-side reshape no longer forces this number up.
pub const TERMINAL_PROTOCOL_VERSION: u32 = 18;

/// The oldest terminal-wire version this build is still willing to
/// negotiate down to in [`TerminalHub::hello`] — the low end of the
/// advertised `[min_supported, current]` range.
///
/// Equal to [`TERMINAL_PROTOCOL_VERSION`] under the standing **lockstep, no
/// per-feature gates** policy (owner, 2026-07-30): same-machine
/// self-spawned daemons do not need cross-version interop, they need honest
/// restart, so a mismatched `hello` is rejected and recovered by the
/// client's auto-drain-and-respawn rather than bridged by gate constants.
/// The two such constants this wire once had (`SCROLLBACK_WINDOW_MIN_VERSION`
/// = 12, `TERMINAL_STRUCTURED_INPUT_VERSION` = 13) were deleted with the
/// phase-2 split: under a floor of 17+ their `>=` comparisons can never be
/// false (`docs/runtime-granularity-design.md` Q4). One shell-side arm
/// outlived them — the structured-input path still checks whether its
/// terminal runtime has negotiated *at all*, because a pane can dispatch a
/// keystroke before its first `hello` lands (`terminal::session::
/// version_supports_structured_input`). That is a connection check, not a
/// version gate.
pub const MIN_SUPPORTED_TERMINAL_PROTOCOL_VERSION: u32 = 18;

/// The version range this build advertises in every `hello` to
/// `horizon-terminald`.
///
/// The range *type* is domain-free ([`VersionRange`]); which numbers go in
/// it is not, which is why this constructor sits beside this hub's own two
/// constants rather than on the type. `horizon_agent::wire::
/// agent_version_range` is its agent-side twin.
pub fn terminal_version_range() -> VersionRange {
    VersionRange::new(
        MIN_SUPPORTED_TERMINAL_PROTOCOL_VERSION,
        TERMINAL_PROTOCOL_VERSION,
    )
}

/// A [`ClientHello`] advertising [`terminal_version_range`] under
/// `binary_id` — what every client sends as the first call on a connection
/// to `horizon-terminald`.
pub fn terminal_client_hello(binary_id: impl Into<String>) -> ClientHello {
    ClientHello::new(terminal_version_range(), binary_id)
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
//
// The `[`HubHello`]` link above dangles on purpose: that type is
// `horizon-agent`'s, a crate this one must never depend on. The wording is
// pinned byte-for-byte by the committed wire-schema artifact (it is this
// type's `description`), so it stays exactly as written — the same rule
// `horizon_wire::negotiate` records for its own types.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TerminalHubHello {
    /// The highest mutually supported version (see [`HubHello::negotiated`]).
    pub negotiated: u32,
    pub binary_id: String,
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
//
// The `SessionHub` links above name the *pre-v17* owner of these methods
// and are frozen for the same artifact-description reason as
// `TerminalHubHello`'s: they live on [`TerminalHub`] now.
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

/// The terminal hub — `horizon-terminald`'s whole rtc surface
/// (`docs/terminald-split-design.md` decision 1). Carved off `SessionHub`
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
/// does on the agent hub.
#[rtc::remote]
pub trait TerminalHub {
    /// Version negotiation, identical in shape to `SessionHub::hello` —
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
    /// `SessionHub::drain` the call itself typically errors because the
    /// process is gone before a reply can travel.
    async fn drain(&self) -> Result<(), HubError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_range_negotiates_with_itself_at_the_current_version() {
        assert_eq!(
            terminal_version_range().negotiate(terminal_version_range()),
            Some(TERMINAL_PROTOCOL_VERSION)
        );
    }

    /// The mirror of `horizon_agent::wire`'s own assertion: the two hubs'
    /// version pairs are independent constants but start equal, because
    /// the phase-2 split is a crate reorganization and not a wire event.
    /// Neither crate may name the other, so each pins its half against the
    /// literal 18.
    #[test]
    fn the_split_started_at_the_pre_split_version() {
        assert_eq!(TERMINAL_PROTOCOL_VERSION, 18);
        assert_eq!(MIN_SUPPORTED_TERMINAL_PROTOCOL_VERSION, 18);
    }

    /// [`TerminalHub`]'s mechanical method-surface snapshot — the guard
    /// behind the v17 split's central claim that the terminal methods moved
    /// *verbatim*: the same names, the same argument names, on the daemon
    /// that now owns them. Read off the serde shape of the rtc macro's
    /// generated request enum, so renaming a method or an argument goes red
    /// and the schema artifact cannot silently drift from the real trait.
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
}
